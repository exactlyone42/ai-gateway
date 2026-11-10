use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const JOURNAL_MAGIC: &[u8; 8] = b"APGEJ001";
const WITNESS_MAGIC: &[u8; 8] = b"APGEW001";
const HEADER_SIZE: usize = 24;
const RECORD_SIZE: usize = 104;
const WITNESS_SIZE: usize = 96;
const MAX_RECORDS: usize = 100_000;

pub(crate) struct ErasureJournal {
    journal: PathBuf,
    journal_dir: File,
    witness: PathBuf,
    witness_dir: File,
    journal_identity: (u64, u64),
    witness_identity: (u64, u64),
    events_identity: (u64, u64),
    remote: Option<std::sync::Arc<crate::capture_remote_witness::RemoteWitness>>,
}

struct Verified {
    install: [u8; 16],
    sequence: u64,
    tip: [u8; 32],
    records: Vec<(Uuid, Uuid)>,
}

#[derive(Debug, PartialEq, Eq)]
enum Recovery {
    None,
    CompleteRecord {
        record: [u8; RECORD_SIZE],
        remainder: usize,
    },
    ClearPending,
    AdvanceHead,
}

struct Inspection {
    verified: Verified,
    recovery: Recovery,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ErasurePosition {
    pub(crate) install: Uuid,
    pub(crate) sequence: u64,
    pub(crate) tip: [u8; 32],
}

impl ErasurePosition {
    pub(crate) fn from_database(install: Uuid, sequence: i64, tip: Vec<u8>) -> io::Result<Self> {
        Ok(Self {
            install,
            sequence: u64::try_from(sequence).map_err(|_| invalid())?,
            tip: tip.try_into().map_err(|_| invalid())?,
        })
    }
}

pub(crate) fn inspect_position(
    journal: &Path,
    witness: &Path,
    expected: Option<&ErasurePosition>,
) -> io::Result<ErasurePosition> {
    let store = ErasureJournal::open_unverified(journal, witness)?;
    inspect_store(&store, expected)
}

fn inspect_store(
    store: &ErasureJournal,
    expected: Option<&ErasurePosition>,
) -> io::Result<ErasurePosition> {
    let lock = journal_lock(&store.journal_dir)?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => {
            io::Error::new(io::ErrorKind::WouldBlock, "erasure journal is in use")
        }
        std::fs::TryLockError::Error(error) => error,
    })?;
    let _lease = LockLease { file: lock };
    let mut events = child_file_read(&store.journal_dir, "events")?;
    let inspection = store.inspect_recovery(&mut events)?;
    if inspection.recovery != Recovery::None {
        return Err(invalid());
    }
    let current = inspection.verified;
    let actual = ErasurePosition {
        install: Uuid::from_bytes(current.install),
        sequence: current.sequence,
        tip: current.tip,
    };
    if let Some(expected) = expected {
        if expected.install != actual.install || expected.sequence > actual.sequence {
            return Err(invalid());
        }
        let tip = if expected.sequence == 0 {
            events.seek(SeekFrom::Start(0))?;
            let mut header = [0u8; HEADER_SIZE];
            events.read_exact(&mut header)?;
            Sha256::digest(header).into()
        } else {
            let offset = expected
                .sequence
                .checked_sub(1)
                .and_then(|sequence| sequence.checked_mul(RECORD_SIZE as u64))
                .and_then(|offset| offset.checked_add((HEADER_SIZE + 72) as u64))
                .ok_or_else(invalid)?;
            events.seek(SeekFrom::Start(offset))?;
            let mut tip = [0u8; 32];
            events.read_exact(&mut tip)?;
            tip
        };
        if expected.tip != tip {
            return Err(invalid());
        }
    }
    Ok(actual)
}

struct LockLease {
    file: File,
}

pub(crate) struct ActiveErasure {
    journal: ErasureJournal,
    _owner: LockLease,
    remote_gate: Option<std::sync::Arc<tokio::sync::Semaphore>>,
}

impl ActiveErasure {
    pub(crate) fn verify_position(&self, expected: &ErasurePosition) -> io::Result<()> {
        inspect_store(&self.journal, Some(expected)).map(|_| ())
    }

    pub(crate) fn initialize(journal: &Path, witness: &Path) -> io::Result<Self> {
        crate::capture_paths::validate_paths([journal, witness]).map_err(|_| invalid())?;
        validate_owners(journal)?;
        validate_owners(witness)?;
        let witness_dir = directory(witness)?;
        let witness_metadata = witness_dir.metadata()?;
        let owner_file = journal_lock(&witness_dir)?;
        owner_file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => {
                io::Error::new(io::ErrorKind::WouldBlock, "erasure witness is in use")
            }
            std::fs::TryLockError::Error(error) => error,
        })?;
        let owner = LockLease { file: owner_file };
        let store = match ErasureJournal::open_unverified(journal, witness) {
            Ok(store)
                if read_optional(&witness_dir, "initializing", HEADER_SIZE + WITNESS_SIZE)?
                    .is_none() =>
            {
                store
            }
            _ => ErasureJournal::initialize(journal, witness)?,
        };
        if store.witness_identity != (witness_metadata.dev(), witness_metadata.ino())
            || !store.verified_records()?.is_empty()
        {
            return Err(invalid());
        }
        Ok(Self {
            journal: store,
            _owner: owner,
            remote_gate: None,
        })
    }

    pub(crate) fn open(journal: &Path, witness: &Path) -> io::Result<Self> {
        Self::open_with_remote(journal, witness, None)
    }

    pub(crate) fn open_with_remote(
        journal: &Path,
        witness: &Path,
        remote: Option<std::sync::Arc<crate::capture_remote_witness::RemoteWitness>>,
    ) -> io::Result<Self> {
        let mut journal = ErasureJournal::open_unverified(journal, witness)?;
        let owner = journal.claim_owner()?;
        let remote_gate = remote
            .as_ref()
            .map(|_| std::sync::Arc::new(tokio::sync::Semaphore::new(4)));
        journal.remote = remote;
        journal.verified_records()?;
        Ok(Self {
            journal,
            _owner: owner,
            remote_gate,
        })
    }

    pub(crate) fn append(&self, tenant: Uuid, capture: Uuid) -> io::Result<()> {
        self.journal.append(tenant, capture)
    }

    pub(crate) fn try_remote_permit(
        &self,
    ) -> io::Result<Option<tokio::sync::OwnedSemaphorePermit>> {
        self.remote_gate
            .as_ref()
            .map(|gate| {
                gate.clone().try_acquire_owned().map_err(|_| {
                    io::Error::new(io::ErrorKind::WouldBlock, "remote erasure witness busy")
                })
            })
            .transpose()
    }

    pub(crate) fn verified_records(&self) -> io::Result<Vec<(Uuid, Uuid)>> {
        self.journal.verified_records()
    }
}

impl Drop for LockLease {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            tracing::error!(error.type = "erasure_lock_unlock_failed", exception.message = %error);
        }
    }
}

#[cfg(test)]
mod test_support {
    use super::*;
    pub(crate) struct TestErasure {
        pub(crate) active: std::sync::Arc<ActiveErasure>,
        root: PathBuf,
    }

    impl TestErasure {
        pub(crate) fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let root = Path::new("/tmp").join(format!("gateway-test-erasure-{}", Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            let journal = root.join("journal");
            let witness = root.join("witness");
            for path in [&journal, &witness] {
                std::fs::create_dir(path).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self {
                active: std::sync::Arc::new(ActiveErasure::initialize(&journal, &witness).unwrap()),
                root,
            }
        }
    }

    impl Drop for TestErasure {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
}

#[cfg(test)]
pub(crate) use test_support::TestErasure;

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid erasure journal")
}

fn validate_owners(path: &Path) -> io::Result<()> {
    let current = unsafe { libc::geteuid() };
    for component in path.ancestors() {
        let owner = std::fs::metadata(component)?.uid();
        if owner != 0 && owner != current {
            return Err(invalid());
        }
    }
    Ok(())
}

fn directory_identity(path: &Path) -> io::Result<(u64, u64)> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(invalid());
    }
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(test)]
fn file(path: &Path, create: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    if create {
        options.create_new(true);
    }
    check_file(options.open(path)?)
}

fn check_file(file: File) -> io::Result<File> {
    let metadata = file.metadata()?;
    let current = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
        || (metadata.uid() != 0 && metadata.uid() != current)
    {
        return Err(invalid());
    }
    Ok(file)
}

fn child_file(dir: &File, name: &str, create: bool) -> io::Result<File> {
    let mut flags = libc::O_RDWR;
    if create {
        flags |= libc::O_CREAT | libc::O_EXCL;
    }
    open_child_file(dir, name, flags)
}

fn child_file_read(dir: &File, name: &str) -> io::Result<File> {
    open_child_file(dir, name, libc::O_RDONLY)
}

fn open_child_file(dir: &File, name: &str, flags: libc::c_int) -> io::Result<File> {
    let name = CString::new(name).map_err(|_| invalid())?;
    let flags = flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags, 0o600) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    check_file(unsafe { File::from_raw_fd(fd) })
}

fn rename_child(dir: &File, from: &str, to: &str, replace: bool) -> io::Result<()> {
    let from = CString::new(from).map_err(|_| invalid())?;
    let to = CString::new(to).map_err(|_| invalid())?;
    let flags = if replace { 0 } else { libc::RENAME_NOREPLACE };
    if unsafe {
        libc::renameat2(
            dir.as_raw_fd(),
            from.as_ptr(),
            dir.as_raw_fd(),
            to.as_ptr(),
            flags,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    dir.sync_all()
}

fn remove_child(dir: &File, name: &str) -> io::Result<()> {
    let name = CString::new(name).map_err(|_| invalid())?;
    if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    dir.sync_all()
}

fn directory(path: &Path) -> io::Result<File> {
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = dir.metadata()?;
    if (metadata.dev(), metadata.ino()) != directory_identity(path)? {
        return Err(invalid());
    }
    Ok(dir)
}

fn read_optional(dir: &File, name: &str, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut item = match child_file_read(dir, name) {
        Ok(item) => item,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if item.metadata()?.len() > max as u64 {
        return Err(invalid());
    }
    let mut data = Vec::new();
    item.read_to_end(&mut data)?;
    Ok(Some(data))
}

fn complete_initial_file(
    dir: &File,
    name: &str,
    expected: &[u8],
    existing: Option<Vec<u8>>,
) -> io::Result<()> {
    let offset = existing.as_ref().map_or(0, Vec::len);
    let mut item = child_file(dir, name, existing.is_none())?;
    item.seek(SeekFrom::End(0))?;
    item.write_all(&expected[offset..])?;
    item.sync_all()?;
    dir.sync_all()
}

fn journal_lock(dir: &File) -> io::Result<File> {
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn witness_bytes(install: &[u8; 16], sequence: u64, tip: &[u8; 32]) -> Vec<u8> {
    let mut output = Vec::with_capacity(WITNESS_SIZE);
    output.extend_from_slice(WITNESS_MAGIC);
    output.extend_from_slice(install);
    output.extend_from_slice(&sequence.to_be_bytes());
    output.extend_from_slice(tip);
    output.extend_from_slice(&Sha256::digest(&output));
    output
}

fn read_witness(dir: &File, install: &[u8; 16]) -> io::Result<(u64, [u8; 32])> {
    let mut head = child_file_read(dir, "head")?;
    if head.metadata()?.len() != WITNESS_SIZE as u64 {
        return Err(invalid());
    }
    let mut actual = [0u8; WITNESS_SIZE];
    head.read_exact(&mut actual)?;
    let sequence = u64::from_be_bytes(actual[24..32].try_into().map_err(|_| invalid())?);
    let tip: [u8; 32] = actual[32..64].try_into().map_err(|_| invalid())?;
    if actual.as_slice() != witness_bytes(install, sequence, &tip) {
        return Err(invalid());
    }
    Ok((sequence, tip))
}

fn read_pending(dir: &File, install: &[u8; 16]) -> io::Result<(u64, [u8; 32], [u8; RECORD_SIZE])> {
    let mut pending = child_file_read(dir, "pending")?;
    if pending.metadata()?.len() != (WITNESS_SIZE + RECORD_SIZE) as u64 {
        return Err(invalid());
    }
    let mut data = [0u8; WITNESS_SIZE + RECORD_SIZE];
    pending.read_exact(&mut data)?;
    let sequence = u64::from_be_bytes(data[24..32].try_into().map_err(|_| invalid())?);
    let tip: [u8; 32] = data[32..64].try_into().map_err(|_| invalid())?;
    let record: [u8; RECORD_SIZE] = data[WITNESS_SIZE..].try_into().map_err(|_| invalid())?;
    let mut digest = Sha256::new();
    digest.update(b"APGER001");
    digest.update(install);
    digest.update(&record[..72]);
    let actual: [u8; 32] = digest.finalize().into();
    if data[..WITNESS_SIZE] != witness_bytes(install, sequence, &tip)
        || record[..8] != sequence.to_be_bytes()
        || record[72..] != tip
        || actual != tip
    {
        return Err(invalid());
    }
    Ok((sequence, tip, record))
}

impl ErasureJournal {
    fn claim_owner(&self) -> io::Result<LockLease> {
        let owner = journal_lock(&self.witness_dir)?;
        owner.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => {
                io::Error::new(io::ErrorKind::WouldBlock, "erasure witness is in use")
            }
            std::fs::TryLockError::Error(error) => error,
        })?;
        let owner = LockLease { file: owner };
        if directory_identity(&self.witness)? != self.witness_identity
            || directory_identity(&self.journal)? != self.journal_identity
        {
            return Err(invalid());
        }
        Ok(owner)
    }

    pub(crate) fn initialize(journal: &Path, witness: &Path) -> io::Result<Self> {
        crate::capture_paths::validate_paths([journal, witness]).map_err(|_| invalid())?;
        validate_owners(journal)?;
        validate_owners(witness)?;
        let journal_dir = directory(journal)?;
        let witness_dir = directory(witness)?;
        let lock = journal_lock(&journal_dir)?;
        lock.lock()?;
        let _lease = LockLease { file: lock };
        let intent = match read_optional(&witness_dir, "initializing", HEADER_SIZE + WITNESS_SIZE)?
        {
            Some(intent) => intent,
            None => {
                if read_optional(&journal_dir, "events", HEADER_SIZE)?.is_some()
                    || read_optional(&witness_dir, "head", WITNESS_SIZE)?.is_some()
                    || read_optional(
                        &witness_dir,
                        "pending",
                        HEADER_SIZE + WITNESS_SIZE + RECORD_SIZE,
                    )?
                    .is_some()
                {
                    return Err(invalid());
                }
                let install = *Uuid::new_v4().as_bytes();
                let mut header = Vec::with_capacity(HEADER_SIZE);
                header.extend_from_slice(JOURNAL_MAGIC);
                header.extend_from_slice(&install);
                let tip: [u8; 32] = Sha256::digest(&header).into();
                let mut intent = header;
                intent.extend_from_slice(&witness_bytes(&install, 0, &tip));
                let temporary = format!(".initializing-{}", Uuid::new_v4());
                let mut pending = child_file(&witness_dir, &temporary, true)?;
                pending.write_all(&intent)?;
                pending.sync_all()?;
                rename_child(&witness_dir, &temporary, "initializing", false)?;
                intent
            }
        };
        if intent.len() != HEADER_SIZE + WITNESS_SIZE || &intent[..8] != JOURNAL_MAGIC {
            return Err(invalid());
        }
        let header = &intent[..HEADER_SIZE];
        let install: [u8; 16] = header[8..].try_into().map_err(|_| invalid())?;
        let tip: [u8; 32] = Sha256::digest(header).into();
        let head = witness_bytes(&install, 0, &tip);
        if intent[HEADER_SIZE..] != head {
            return Err(invalid());
        }
        let existing_events = read_optional(&journal_dir, "events", HEADER_SIZE)?;
        let existing_head = read_optional(&witness_dir, "head", WITNESS_SIZE)?;
        if existing_events
            .as_ref()
            .is_some_and(|bytes| !header.starts_with(bytes))
            || existing_head
                .as_ref()
                .is_some_and(|bytes| !head.starts_with(bytes))
            || read_optional(&witness_dir, "pending", WITNESS_SIZE + RECORD_SIZE)?.is_some()
        {
            return Err(invalid());
        }
        complete_initial_file(&journal_dir, "events", header, existing_events)?;
        complete_initial_file(&witness_dir, "head", &head, existing_head)?;
        remove_child(&witness_dir, "initializing")?;
        drop(_lease);
        Self::open(journal, witness)
    }

    pub(crate) fn open(journal: &Path, witness: &Path) -> io::Result<Self> {
        let store = Self::open_unverified(journal, witness)?;
        store.verified_records()?;
        Ok(store)
    }

    fn open_unverified(journal: &Path, witness: &Path) -> io::Result<Self> {
        crate::capture_paths::validate_paths([journal, witness]).map_err(|_| invalid())?;
        validate_owners(journal)?;
        validate_owners(witness)?;
        let journal_dir = directory(journal)?;
        let journal_metadata = journal_dir.metadata()?;
        let witness_dir = directory(witness)?;
        let witness_metadata = witness_dir.metadata()?;
        let events_metadata = child_file_read(&journal_dir, "events")?.metadata()?;
        Ok(Self {
            journal: journal.to_owned(),
            journal_dir,
            witness: witness.to_owned(),
            witness_dir,
            journal_identity: (journal_metadata.dev(), journal_metadata.ino()),
            witness_identity: (witness_metadata.dev(), witness_metadata.ino()),
            events_identity: (events_metadata.dev(), events_metadata.ino()),
            remote: None,
        })
    }

    fn inspect_recovery(&self, events: &mut File) -> io::Result<Inspection> {
        if read_optional(
            &self.witness_dir,
            "initializing",
            HEADER_SIZE + WITNESS_SIZE,
        )?
        .is_some()
        {
            return Err(invalid());
        }
        let events_metadata = events.metadata()?;
        let path_metadata = child_file_read(&self.journal_dir, "events")?.metadata()?;
        if directory_identity(&self.journal)? != self.journal_identity
            || directory_identity(&self.witness)? != self.witness_identity
            || (events_metadata.dev(), events_metadata.ino()) != self.events_identity
            || (path_metadata.dev(), path_metadata.ino()) != self.events_identity
        {
            return Err(invalid());
        }
        let size = usize::try_from(events.metadata()?.len()).map_err(|_| invalid())?;
        if size < HEADER_SIZE || (size - HEADER_SIZE) / RECORD_SIZE > MAX_RECORDS {
            return Err(invalid());
        }
        events.seek(SeekFrom::Start(0))?;
        let mut header = [0u8; HEADER_SIZE];
        events.read_exact(&mut header)?;
        if &header[..8] != JOURNAL_MAGIC {
            return Err(invalid());
        }
        let install: [u8; 16] = header[8..].try_into().map_err(|_| invalid())?;
        let mut tip: [u8; 32] = Sha256::digest(header).into();
        let mut previous_tip = tip;
        let count = (size - HEADER_SIZE) / RECORD_SIZE;
        let mut records = Vec::with_capacity(count);
        for index in 0..count {
            let mut record = [0u8; RECORD_SIZE];
            events.read_exact(&mut record)?;
            let sequence = u64::from_be_bytes(record[..8].try_into().map_err(|_| invalid())?);
            if sequence != index as u64 + 1 || record[40..72] != tip {
                return Err(invalid());
            }
            let mut digest = Sha256::new();
            digest.update(b"APGER001");
            digest.update(install);
            digest.update(&record[..72]);
            let expected: [u8; 32] = digest.finalize().into();
            if record[72..] != expected {
                return Err(invalid());
            }
            let tenant = Uuid::from_slice(&record[8..24]).map_err(|_| invalid())?;
            let capture = Uuid::from_slice(&record[24..40]).map_err(|_| invalid())?;
            records.push((tenant, capture));
            previous_tip = tip;
            tip = expected;
        }
        let sequence = u64::try_from(count).map_err(|_| invalid())?;
        let (head_sequence, head_tip) = read_witness(&self.witness_dir, &install)?;
        let pending = match read_pending(&self.witness_dir, &install) {
            Ok(pending) => Some(pending),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let remainder = (size - HEADER_SIZE) % RECORD_SIZE;
        let recovery = if let Some((pending_sequence, _, record)) = pending
            && (sequence, tip) == (head_sequence, head_tip)
            && head_sequence.checked_add(1) == Some(pending_sequence)
            && record[40..72] == tip
        {
            if count >= MAX_RECORDS {
                return Err(invalid());
            }
            let mut partial = vec![0u8; remainder];
            events.read_exact(&mut partial)?;
            if partial != record[..remainder] {
                return Err(invalid());
            }
            Recovery::CompleteRecord { record, remainder }
        } else {
            if remainder != 0 {
                return Err(invalid());
            }
            match pending {
                None if (sequence, tip) == (head_sequence, head_tip) => Recovery::None,
                Some((pending_sequence, pending_tip, _))
                    if (sequence, tip) == (head_sequence, head_tip)
                        && (pending_sequence, pending_tip) == (sequence, tip) =>
                {
                    Recovery::ClearPending
                }
                Some((pending_sequence, pending_tip, _))
                    if (sequence, tip) == (pending_sequence, pending_tip)
                        && head_sequence.checked_add(1) == Some(sequence)
                        && previous_tip == head_tip =>
                {
                    Recovery::AdvanceHead
                }
                _ => return Err(invalid()),
            }
        };
        Ok(Inspection {
            verified: Verified {
                install,
                sequence,
                tip,
                records,
            },
            recovery,
        })
    }

    fn verified(&self, events: &mut File) -> io::Result<Verified> {
        let inspection = self.inspect_recovery(events)?;
        self.check_remote(&inspection)?;
        match inspection.recovery {
            Recovery::None => Ok(inspection.verified),
            Recovery::CompleteRecord { record, remainder } => {
                events.seek(SeekFrom::End(0))?;
                events.write_all(&record[remainder..])?;
                events.sync_all()?;
                let completed = self.inspect_recovery(events)?;
                if completed.recovery != Recovery::AdvanceHead {
                    return Err(invalid());
                }
                let current = completed.verified;
                self.replace_head(&current.install, current.sequence, &current.tip)?;
                self.clear_pending()?;
                Ok(current)
            }
            Recovery::ClearPending => {
                self.clear_pending()?;
                Ok(inspection.verified)
            }
            Recovery::AdvanceHead => {
                let current = inspection.verified;
                self.replace_head(&current.install, current.sequence, &current.tip)?;
                self.clear_pending()?;
                Ok(current)
            }
        }
    }

    fn advance_remote(&self, current: &ErasurePosition, next: &ErasurePosition) -> io::Result<()> {
        let Some(remote) = &self.remote else {
            return Ok(());
        };
        if remote.advance(current, next)? || remote.read()? == *next {
            return Ok(());
        }
        Err(invalid())
    }

    fn check_remote(&self, inspection: &Inspection) -> io::Result<()> {
        let Some(remote) = &self.remote else {
            return Ok(());
        };
        let current = ErasurePosition {
            install: Uuid::from_bytes(inspection.verified.install),
            sequence: inspection.verified.sequence,
            tip: inspection.verified.tip,
        };
        let observed = remote.read()?;
        if let Recovery::CompleteRecord { record, .. } = &inspection.recovery {
            let next = ErasurePosition {
                install: current.install,
                sequence: u64::from_be_bytes(record[..8].try_into().map_err(|_| invalid())?),
                tip: record[72..].try_into().map_err(|_| invalid())?,
            };
            if observed == next {
                return Ok(());
            }
            if observed == current {
                return self.advance_remote(&current, &next);
            }
            return Err(invalid());
        }
        if observed != current {
            return Err(invalid());
        }
        Ok(())
    }

    pub(crate) fn verified_records(&self) -> io::Result<Vec<(Uuid, Uuid)>> {
        let lock = journal_lock(&self.journal_dir)?;
        lock.lock()?;
        let _lease = LockLease { file: lock };
        let mut events = child_file(&self.journal_dir, "events", false)?;
        Ok(self.verified(&mut events)?.records)
    }

    fn replace_head(&self, install: &[u8; 16], sequence: u64, tip: &[u8; 32]) -> io::Result<()> {
        let temporary = format!(".head-{}", Uuid::new_v4());
        let mut head = child_file(&self.witness_dir, &temporary, true)?;
        head.write_all(&witness_bytes(install, sequence, tip))?;
        head.sync_all()?;
        rename_child(&self.witness_dir, &temporary, "head", true)
    }

    fn clear_pending(&self) -> io::Result<()> {
        remove_child(&self.witness_dir, "pending")
    }

    pub(crate) fn append(&self, tenant: Uuid, capture: Uuid) -> io::Result<()> {
        let lock = journal_lock(&self.journal_dir)?;
        lock.lock()?;
        let _lease = LockLease { file: lock };
        let mut events = child_file(&self.journal_dir, "events", false)?;
        let current = self.verified(&mut events)?;
        if current.records.contains(&(tenant, capture)) {
            return Ok(());
        }
        if current.records.len() >= MAX_RECORDS {
            return Err(invalid());
        }
        let sequence = current.sequence.checked_add(1).ok_or_else(invalid)?;
        let mut record = Vec::with_capacity(RECORD_SIZE);
        record.extend_from_slice(&sequence.to_be_bytes());
        record.extend_from_slice(tenant.as_bytes());
        record.extend_from_slice(capture.as_bytes());
        record.extend_from_slice(&current.tip);
        let mut digest = Sha256::new();
        digest.update(b"APGER001");
        digest.update(current.install);
        digest.update(&record);
        let tip: [u8; 32] = digest.finalize().into();
        record.extend_from_slice(&tip);
        let temporary_pending = format!(".pending-{}", Uuid::new_v4());
        let mut pending = child_file(&self.witness_dir, &temporary_pending, true)?;
        pending.write_all(&witness_bytes(&current.install, sequence, &tip))?;
        pending.write_all(&record)?;
        pending.sync_all()?;
        rename_child(&self.witness_dir, &temporary_pending, "pending", false)?;
        let prior = ErasurePosition {
            install: Uuid::from_bytes(current.install),
            sequence: current.sequence,
            tip: current.tip,
        };
        let next = ErasurePosition {
            install: prior.install,
            sequence,
            tip,
        };
        self.advance_remote(&prior, &next)?;
        events.seek(SeekFrom::End(0))?;
        events.write_all(&record)?;
        events.sync_all()?;
        self.replace_head(&current.install, sequence, &tip)?;
        self.clear_pending()
    }
}

#[cfg(test)]
mod tests {
    use super::ErasureJournal;
    use std::os::unix::fs::PermissionsExt;
    use uuid::Uuid;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "需要隔離見證資料庫"]
    async fn remote_witness_gates_recovery_and_duplicate_deletions() {
        use std::str::FromStr;

        let url = std::env::var("TEST_ERASURE_WITNESS_URL").unwrap();
        let admin_url = std::env::var("TEST_ERASURE_WITNESS_ADMIN_URL").unwrap();
        let options = sqlx::postgres::PgConnectOptions::from_str(&url).unwrap();
        let admin_options = sqlx::postgres::PgConnectOptions::from_str(&admin_url).unwrap();
        assert_eq!(options.get_host(), "127.0.0.1");
        assert_eq!(admin_options.get_host(), options.get_host());
        assert_eq!(admin_options.get_port(), options.get_port());
        assert_eq!(admin_options.get_database(), options.get_database());
        assert_ne!(admin_options.get_username(), options.get_username());
        assert!(matches!(
            options.get_ssl_mode(),
            sqlx::postgres::PgSslMode::VerifyFull
        ));
        assert!(matches!(
            admin_options.get_ssl_mode(),
            sqlx::postgres::PgSslMode::VerifyFull
        ));
        assert!(
            options
                .get_database()
                .is_some_and(|name| name.starts_with("gateway_witness_test_"))
        );
        let pool = sqlx::PgPool::connect(&admin_url).await.unwrap();
        let (root, journal, witness) = directories();
        let mut store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let initial = super::inspect_position(&journal, &witness, None).unwrap();
        sqlx::query_file!(
            "queries/test_erasure_witness_seed.sql",
            initial.install,
            i64::try_from(initial.sequence).unwrap(),
            &initial.tip[..]
        )
        .execute(&pool)
        .await
        .unwrap();
        let remote = std::sync::Arc::new(
            crate::capture_remote_witness::RemoteWitness::new(
                &url,
                "postgres://gateway@primary.invalid/gateway",
            )
            .unwrap(),
        );
        let mut unavailable_url = reqwest::Url::parse(&url).unwrap();
        unavailable_url.set_port(Some(1)).unwrap();
        let unavailable = std::sync::Arc::new(
            crate::capture_remote_witness::RemoteWitness::new(
                unavailable_url.as_str(),
                "postgres://gateway@primary.invalid/gateway",
            )
            .unwrap(),
        );
        let bad_certificate = std::sync::Arc::new(
            crate::capture_remote_witness::RemoteWitness::new(
                &std::env::var("TEST_ERASURE_WITNESS_BAD_URL").unwrap(),
                "postgres://gateway@primary.invalid/gateway",
            )
            .unwrap(),
        );
        store.remote = Some(remote.clone());
        let runtime = tokio::runtime::Handle::current();
        let cleanup_pool = pool.clone();
        let cleanup_install = initial.install;
        let result = tokio::task::spawn_blocking(move || {
            let old_events = std::fs::read(journal.join("events")).unwrap();
            let old_head = std::fs::read(witness.join("head")).unwrap();
            let entry = (Uuid::new_v4(), Uuid::new_v4());
            store.append(entry.0, entry.1).unwrap();
            let latest = remote.read().unwrap();
            assert_eq!(latest.sequence, initial.sequence + 1);
            let new_events = std::fs::read(journal.join("events")).unwrap();
            let new_head = std::fs::read(witness.join("head")).unwrap();
            let mut pending = new_head.clone();
            pending.extend_from_slice(&new_events[old_events.len()..]);
            let reset_local = || {
                std::fs::write(journal.join("events"), &old_events).unwrap();
                std::fs::write(witness.join("head"), &old_head).unwrap();
                std::fs::write(witness.join("pending"), &pending).unwrap();
                std::fs::set_permissions(
                    witness.join("pending"),
                    std::fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            };
            let set_remote = |position: &super::ErasurePosition| {
                runtime.block_on(async {
                    sqlx::query_file!(
                        "queries/test_erasure_witness_set.sql",
                        position.install,
                        i64::try_from(position.sequence).unwrap(),
                        &position.tip[..]
                    )
                    .execute(&pool)
                    .await
                    .unwrap();
                });
            };
            reset_local();
            assert_eq!(store.verified_records().unwrap(), vec![entry]);
            assert_eq!(remote.read().unwrap(), latest);
            reset_local();
            set_remote(&initial);
            assert_eq!(store.verified_records().unwrap(), vec![entry]);
            assert_eq!(remote.read().unwrap(), latest);
            reset_local();
            std::fs::write(journal.join("events"), &new_events).unwrap();
            set_remote(&initial);
            assert!(store.verified_records().is_err());
            assert_eq!(std::fs::read(witness.join("head")).unwrap(), old_head);
            assert_eq!(std::fs::read(witness.join("pending")).unwrap(), pending);
            set_remote(&latest);
            assert_eq!(store.verified_records().unwrap(), vec![entry]);
            reset_local();
            std::fs::write(journal.join("events"), &new_events).unwrap();
            std::fs::write(witness.join("head"), &new_head).unwrap();
            set_remote(&initial);
            assert!(store.verified_records().is_err());
            assert_eq!(std::fs::read(witness.join("pending")).unwrap(), pending);
            set_remote(&latest);
            assert_eq!(store.verified_records().unwrap(), vec![entry]);
            reset_local();
            let mut fork = initial.clone();
            fork.tip[0] ^= 1;
            set_remote(&fork);
            assert!(store.verified_records().is_err());
            assert_eq!(std::fs::read(journal.join("events")).unwrap(), old_events);
            assert_eq!(std::fs::read(witness.join("head")).unwrap(), old_head);
            assert_eq!(std::fs::read(witness.join("pending")).unwrap(), pending);
            assert!(
                super::ActiveErasure::open_with_remote(&journal, &witness, Some(remote.clone()))
                    .is_err()
            );
            assert_eq!(std::fs::read(journal.join("events")).unwrap(), old_events);
            assert_eq!(std::fs::read(witness.join("pending")).unwrap(), pending);
            set_remote(&latest);
            assert_eq!(remote.read().unwrap(), latest);
            store.remote = Some(unavailable);
            assert!(store.verified_records().is_err());
            assert!(store.append(Uuid::new_v4(), Uuid::new_v4()).is_err());
            assert_eq!(std::fs::read(journal.join("events")).unwrap(), old_events);
            assert_eq!(std::fs::read(witness.join("pending")).unwrap(), pending);
            store.remote = Some(bad_certificate);
            assert!(store.verified_records().is_err());
            assert_eq!(std::fs::read(journal.join("events")).unwrap(), old_events);
            assert_eq!(std::fs::read(witness.join("pending")).unwrap(), pending);
            store.remote = Some(remote.clone());
            std::fs::write(journal.join("events"), &new_events).unwrap();
            std::fs::write(witness.join("head"), &new_head).unwrap();
            std::fs::remove_file(witness.join("pending")).unwrap();
            set_remote(&initial);
            assert!(store.append(entry.0, entry.1).is_err());
            assert_eq!(std::fs::read(journal.join("events")).unwrap(), new_events);
            assert_eq!(std::fs::read(witness.join("head")).unwrap(), new_head);
            assert!(
                super::ActiveErasure::open_with_remote(&journal, &witness, Some(remote.clone()))
                    .is_err()
            );
            set_remote(&latest);
            let active =
                super::ActiveErasure::open_with_remote(&journal, &witness, Some(remote.clone()))
                    .unwrap();
            assert_eq!(active.verified_records().unwrap(), vec![entry]);
            drop(active);
        })
        .await;
        let cleanup = sqlx::query_file!("queries/test_erasure_witness_reset.sql", cleanup_install)
            .execute(&cleanup_pool)
            .await;
        let directory_cleanup = std::fs::remove_dir_all(root);
        result.unwrap();
        cleanup.unwrap();
        directory_cleanup.unwrap();
    }

    #[test]
    fn lease_releases_journal_lock_even_if_a_descriptor_is_duplicated() {
        let (root, journal, _) = directories();
        let directory = super::directory(&journal).unwrap();
        let lock = super::journal_lock(&directory).unwrap();
        lock.lock().unwrap();
        let inherited = lock.try_clone().unwrap();
        drop(lock);
        let other = super::journal_lock(&directory).unwrap();
        assert!(other.try_lock().is_err());
        drop(inherited);
        other.try_lock().unwrap();
        let inherited = other.try_clone().unwrap();
        let lease = super::LockLease { file: other };
        drop(lease);
        let next = super::journal_lock(&directory).unwrap();
        next.try_lock().unwrap();
        next.unlock().unwrap();
        drop(inherited);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn owned_position_check_rejects_replaced_journal_directory() {
        let (root, journal, witness) = directories();
        let active = super::ActiveErasure::initialize(&journal, &witness).unwrap();
        let expected = super::inspect_position(&journal, &witness, None).unwrap();
        let held = root.join("held-journal");
        std::fs::rename(&journal, &held).unwrap();
        std::fs::create_dir(&journal).unwrap();
        std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::copy(held.join("events"), journal.join("events")).unwrap();
        assert!(super::inspect_position(&journal, &witness, Some(&expected)).is_ok());
        assert!(active.verify_position(&expected).is_err());
        drop(active);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn position_accepts_only_verified_ancestors_and_never_repairs_pending() {
        let (root, journal, witness) = directories();
        let active = super::ActiveErasure::initialize(&journal, &witness).unwrap();
        let initial = super::inspect_position(&journal, &witness, None).unwrap();
        let held = super::journal_lock(&super::directory(&journal).unwrap()).unwrap();
        held.try_lock().unwrap();
        let error = super::inspect_position(&journal, &witness, None).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        drop(held);
        active.append(Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let first = super::inspect_position(&journal, &witness, None).unwrap();
        active.append(Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let latest = super::inspect_position(&journal, &witness, Some(&first)).unwrap();
        assert_eq!(latest.sequence, 2);
        assert_eq!(
            super::inspect_position(&journal, &witness, Some(&initial)).unwrap(),
            latest
        );
        let mut wrong = first.clone();
        wrong.install = Uuid::new_v4();
        assert!(super::inspect_position(&journal, &witness, Some(&wrong)).is_err());
        wrong = first.clone();
        wrong.sequence = 3;
        assert!(super::inspect_position(&journal, &witness, Some(&wrong)).is_err());
        wrong = first;
        wrong.tip[0] ^= 1;
        assert!(super::inspect_position(&journal, &witness, Some(&wrong)).is_err());
        let events = journal.join("events");
        let intact = std::fs::read(&events).unwrap();
        let mut corrupted = intact.clone();
        corrupted[super::HEADER_SIZE + 8] ^= 1;
        std::fs::write(&events, &corrupted).unwrap();
        assert!(super::inspect_position(&journal, &witness, None).is_err());
        assert_eq!(std::fs::read(&events).unwrap(), corrupted);
        std::fs::write(&events, intact).unwrap();
        let pending = witness.join("pending");
        std::fs::write(&pending, b"unfinished").unwrap();
        std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(super::inspect_position(&journal, &witness, None).is_err());
        assert_eq!(std::fs::read(&pending).unwrap(), b"unfinished");
        drop(active);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn directories() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let root = std::path::Path::new("/tmp").join(format!("gateway-erasure-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let journal = root.join("journal");
        let witness = root.join("witness");
        for path in [&journal, &witness] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        (root, journal, witness)
    }

    #[test]
    fn records_are_durable_and_tied_to_the_witness() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let first = (Uuid::new_v4(), Uuid::new_v4());
        let second = (Uuid::new_v4(), Uuid::new_v4());
        store.append(first.0, first.1).unwrap();
        store.append(second.0, second.1).unwrap();
        drop(store);
        let reopened = ErasureJournal::open(&journal, &witness).unwrap();
        assert_eq!(reopened.verified_records().unwrap(), vec![first, second]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_initialization_resumes_only_matching_data() {
        let (seed_root, seed_journal, seed_witness) = directories();
        ErasureJournal::initialize(&seed_journal, &seed_witness).unwrap();
        let header = std::fs::read(seed_journal.join("events")).unwrap();
        let head = std::fs::read(seed_witness.join("head")).unwrap();
        let mut intent = header.clone();
        intent.extend_from_slice(&head);
        for phase in 0..4 {
            let (root, journal, witness) = directories();
            std::fs::write(witness.join("initializing"), &intent).unwrap();
            std::fs::set_permissions(
                witness.join("initializing"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            if phase >= 1 {
                std::fs::write(
                    journal.join("events"),
                    &header[..if phase == 1 { 8 } else { header.len() }],
                )
                .unwrap();
                std::fs::set_permissions(
                    journal.join("events"),
                    std::fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
            if phase >= 2 {
                std::fs::write(
                    witness.join("head"),
                    &head[..if phase == 2 { 8 } else { head.len() }],
                )
                .unwrap();
                std::fs::set_permissions(
                    witness.join("head"),
                    std::fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
            if phase == 3 {
                assert!(ErasureJournal::open(&journal, &witness).is_err());
            }
            let store = ErasureJournal::initialize(&journal, &witness).unwrap();
            assert!(store.verified_records().unwrap().is_empty());
            assert_eq!(std::fs::read(journal.join("events")).unwrap(), header);
            assert_eq!(std::fs::read(witness.join("head")).unwrap(), head);
            assert!(!witness.join("initializing").exists());
            std::fs::remove_dir_all(root).unwrap();
        }
        std::fs::remove_dir_all(seed_root).unwrap();
    }

    #[test]
    fn initialization_rejects_conflicting_or_missing_intent_without_changes() {
        let (seed_root, seed_journal, seed_witness) = directories();
        ErasureJournal::initialize(&seed_journal, &seed_witness).unwrap();
        let header = std::fs::read(seed_journal.join("events")).unwrap();
        let head = std::fs::read(seed_witness.join("head")).unwrap();
        let mut intent = header.clone();
        intent.extend_from_slice(&head);
        for case in 0..5 {
            let (root, journal, witness) = directories();
            let mut events = header.clone();
            let mut current_head = head.clone();
            let mut current_intent = intent.clone();
            match case {
                0 => current_intent[0] ^= 1,
                1 => events[8] ^= 1,
                2 => current_head[8] ^= 1,
                3 => events.push(0),
                _ => {}
            }
            std::fs::write(journal.join("events"), &events).unwrap();
            std::fs::write(witness.join("head"), &current_head).unwrap();
            for path in [journal.join("events"), witness.join("head")] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            if case != 4 {
                std::fs::write(witness.join("initializing"), &current_intent).unwrap();
                std::fs::set_permissions(
                    witness.join("initializing"),
                    std::fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
            assert!(ErasureJournal::initialize(&journal, &witness).is_err());
            assert_eq!(std::fs::read(journal.join("events")).unwrap(), events);
            assert_eq!(std::fs::read(witness.join("head")).unwrap(), current_head);
            std::fs::remove_dir_all(root).unwrap();
        }
        std::fs::remove_dir_all(seed_root).unwrap();
    }

    #[test]
    #[ignore = "requires independently owned test directory"]
    fn untrusted_owner_is_rejected() {
        let path = std::env::var_os("GATEWAY_TEST_UNTRUSTED_WITNESS").unwrap();
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        drop(store);
        assert!(ErasureJournal::open(&journal, &witness).is_ok());
        std::fs::copy(
            witness.join("head"),
            std::path::Path::new(&path).join("head"),
        )
        .unwrap();
        assert!(
            crate::capture_paths::validate_paths([&journal, std::path::Path::new(&path)]).is_err()
        );
        assert_eq!(
            std::fs::read(witness.join("head")).unwrap(),
            std::fs::read(std::path::Path::new(&path).join("head")).unwrap()
        );
        assert!(ErasureJournal::open(&journal, std::path::Path::new(&path)).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retrying_a_record_does_not_append_another_copy() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let entry = (Uuid::new_v4(), Uuid::new_v4());
        store.append(entry.0, entry.1).unwrap();
        let before = std::fs::read(journal.join("events")).unwrap();
        store.append(entry.0, entry.1).unwrap();
        assert_eq!(std::fs::read(journal.join("events")).unwrap(), before);
        assert_eq!(store.verified_records().unwrap(), vec![entry]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replaced_journal_directory_is_rejected() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let original = root.join("original-journal");
        std::fs::rename(&journal, &original).unwrap();
        std::fs::create_dir(&journal).unwrap();
        std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::copy(original.join("events"), journal.join("events")).unwrap();
        assert!(store.append(Uuid::new_v4(), Uuid::new_v4()).is_err());
        assert_eq!(
            std::fs::read(original.join("events")).unwrap(),
            std::fs::read(journal.join("events")).unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replaced_witness_directory_is_rejected() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let original = root.join("original-witness");
        std::fs::rename(&witness, &original).unwrap();
        std::fs::create_dir(&witness).unwrap();
        std::fs::set_permissions(&witness, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::copy(original.join("head"), witness.join("head")).unwrap();
        let events = std::fs::read(journal.join("events")).unwrap();
        assert!(store.append(Uuid::new_v4(), Uuid::new_v4()).is_err());
        assert_eq!(std::fs::read(journal.join("events")).unwrap(), events);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn witness_update_uses_opened_directory_after_path_is_replaced() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let mut events = super::file(&journal.join("events"), false).unwrap();
        events.lock().unwrap();
        let verified = store.verified(&mut events).unwrap();
        let original = root.join("original-witness");
        std::fs::rename(&witness, &original).unwrap();
        std::fs::create_dir(&witness).unwrap();
        std::fs::set_permissions(&witness, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::copy(original.join("head"), witness.join("head")).unwrap();
        let copied = std::fs::read(witness.join("head")).unwrap();
        store
            .replace_head(&verified.install, 123, &[7; 32])
            .unwrap();
        assert_eq!(std::fs::read(witness.join("head")).unwrap(), copied);
        assert_ne!(std::fs::read(original.join("head")).unwrap(), copied);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn publication_never_overwrites_an_existing_pending_record() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        std::fs::write(witness.join("pending"), b"existing").unwrap();
        std::fs::write(witness.join("new-pending"), b"replacement").unwrap();
        assert!(super::rename_child(&store.witness_dir, "new-pending", "pending", false).is_err());
        assert_eq!(std::fs::read(witness.join("pending")).unwrap(), b"existing");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replaced_events_file_is_rejected() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        std::fs::rename(journal.join("events"), journal.join("previous-events")).unwrap();
        std::fs::copy(journal.join("previous-events"), journal.join("events")).unwrap();
        assert!(store.append(Uuid::new_v4(), Uuid::new_v4()).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacing_events_cannot_create_a_second_writer() {
        let (root, journal, witness) = directories();
        let first = ErasureJournal::initialize(&journal, &witness).unwrap();
        let locked = super::journal_lock(&first.journal_dir).unwrap();
        locked.lock().unwrap();
        std::fs::rename(journal.join("events"), journal.join("old-events")).unwrap();
        std::fs::copy(journal.join("old-events"), journal.join("events")).unwrap();
        let second = super::journal_lock(&first.journal_dir).unwrap();
        assert!(second.try_lock().is_err());
        let external = std::process::Command::new("flock")
            .args(["--nonblock", "--exclusive"])
            .arg(&journal)
            .arg("true")
            .status()
            .unwrap();
        assert!(!external.success());
        drop(locked);
        let external = std::process::Command::new("flock")
            .args(["--nonblock", "--exclusive"])
            .arg(&journal)
            .arg("true")
            .status()
            .unwrap();
        assert!(external.success());
        drop(first);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn truncated_log_or_rolled_back_witness_is_rejected() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let old_witness = std::fs::read(witness.join("head")).unwrap();
        store.append(Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let current_witness = std::fs::read(witness.join("head")).unwrap();
        std::fs::write(witness.join("head"), old_witness).unwrap();
        assert!(store.verified_records().is_err());
        std::fs::write(witness.join("head"), current_witness).unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(journal.join("events"))
            .unwrap();
        file.set_len(file.metadata().unwrap().len() - 1).unwrap();
        assert!(store.verified_records().is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_witness_from_another_installation_is_rejected() {
        let (root, journal, witness) = directories();
        let (other_root, other_journal, other_witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        ErasureJournal::initialize(&other_journal, &other_witness).unwrap();
        std::fs::copy(other_witness.join("head"), witness.join("head")).unwrap();
        assert!(store.verified_records().is_err());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(other_root).unwrap();
    }

    #[test]
    fn missing_witness_rejects_new_records_without_changing_the_log() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        store.append(Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let before = std::fs::read(journal.join("events")).unwrap();
        std::fs::rename(witness.join("head"), witness.join("held")).unwrap();
        assert!(store.append(Uuid::new_v4(), Uuid::new_v4()).is_err());
        assert_eq!(std::fs::read(journal.join("events")).unwrap(), before);
        std::fs::rename(witness.join("held"), witness.join("head")).unwrap();
        assert_eq!(store.verified_records().unwrap().len(), 1);
        assert!(ErasureJournal::initialize(&journal, &witness).is_err());
        assert_eq!(store.verified_records().unwrap().len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_deletions_are_recorded_without_losing_entries() {
        let (root, journal, witness) = directories();
        let store = std::sync::Arc::new(ErasureJournal::initialize(&journal, &witness).unwrap());
        let mut workers = Vec::new();
        for _ in 0..8 {
            let store = store.clone();
            workers.push(std::thread::spawn(move || {
                let entry = (Uuid::new_v4(), Uuid::new_v4());
                store.append(entry.0, entry.1).unwrap();
                entry
            }));
        }
        let expected: std::collections::HashSet<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        let actual: std::collections::HashSet<_> =
            store.verified_records().unwrap().into_iter().collect();
        assert_eq!(actual, expected);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_one_gateway_can_own_the_same_witness_directory() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let owner = store.claim_owner().unwrap();
        assert!(store.claim_owner().is_err());
        let other = ErasureJournal::open(&journal, &witness).unwrap();
        assert!(other.claim_owner().is_err());
        let external = std::process::Command::new("flock")
            .args(["--nonblock", "--exclusive"])
            .arg(&witness)
            .arg("true")
            .status()
            .unwrap();
        assert!(!external.success());
        let inherited = owner.file.try_clone().unwrap();
        drop(owner);
        assert!(other.claim_owner().is_ok());
        drop(inherited);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_work_keeps_owner_lock_after_caller_drops() {
        let (root, journal, witness) = directories();
        ErasureJournal::initialize(&journal, &witness).unwrap();
        let active = std::sync::Arc::new(super::ActiveErasure::open(&journal, &witness).unwrap());
        let worker = active.clone();
        let (release, wait) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            wait.recv().unwrap();
            assert!(worker.verified_records().unwrap().is_empty());
        });
        drop(active);
        assert!(super::ActiveErasure::open(&journal, &witness).is_err());
        release.send(()).unwrap();
        handle.join().unwrap();
        assert!(super::ActiveErasure::open(&journal, &witness).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_initialization_resumes_only_empty_journal() {
        let (root, journal, witness) = directories();
        let active = super::ActiveErasure::initialize(&journal, &witness).unwrap();
        assert!(super::ActiveErasure::initialize(&journal, &witness).is_err());
        drop(active);
        let active = super::ActiveErasure::initialize(&journal, &witness).unwrap();
        active.append(Uuid::new_v4(), Uuid::new_v4()).unwrap();
        drop(active);
        assert!(super::ActiveErasure::initialize(&journal, &witness).is_err());
        assert_eq!(
            ErasureJournal::open(&journal, &witness)
                .unwrap()
                .verified_records()
                .unwrap()
                .len(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn contender_cannot_recover_before_claiming_owner() {
        let (root, journal, witness) = directories();
        ErasureJournal::initialize(&journal, &witness).unwrap();
        let active = super::ActiveErasure::open(&journal, &witness).unwrap();
        let old_head = std::fs::read(witness.join("head")).unwrap();
        active.append(Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let new_head = std::fs::read(witness.join("head")).unwrap();
        let events = std::fs::read(journal.join("events")).unwrap();
        let mut pending = new_head;
        pending.extend_from_slice(&events[super::HEADER_SIZE..]);
        std::fs::write(witness.join("head"), &old_head).unwrap();
        std::fs::write(witness.join("pending"), &pending).unwrap();
        std::fs::set_permissions(
            witness.join("pending"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let error = match super::ActiveErasure::open(&journal, &witness) {
            Err(error) => error,
            Ok(_) => panic!("contender unexpectedly acquired ownership"),
        };
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(std::fs::read(witness.join("head")).unwrap(), old_head);
        assert_eq!(std::fs::read(witness.join("pending")).unwrap(), pending);
        drop(active);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inspection_reads_without_write_access_or_file_changes() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;

        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        store.append(Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let events_path = journal.join("events");
        let head_path = witness.join("head");
        let events = std::fs::read(&events_path).unwrap();
        let head = std::fs::read(&head_path).unwrap();
        let mut pending = head.clone();
        pending.extend_from_slice(&events[events.len() - super::RECORD_SIZE..]);
        let pending_path = witness.join("pending");
        std::fs::write(&pending_path, &pending).unwrap();
        let writable = super::child_file(&store.journal_dir, "events", false).unwrap();
        let old_flags = unsafe { libc::fcntl(writable.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(old_flags & libc::O_ACCMODE, libc::O_RDWR);
        drop(writable);
        for path in [&events_path, &head_path, &pending_path] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400)).unwrap();
        }
        if unsafe { libc::geteuid() } != 0 {
            let error = super::child_file(&store.journal_dir, "events", false).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        }
        let before: Vec<_> = [&events_path, &head_path, &pending_path]
            .into_iter()
            .map(|path| {
                let metadata = std::fs::metadata(path).unwrap();
                (
                    metadata.ino(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                    std::fs::read(path).unwrap(),
                )
            })
            .collect();
        let mut input = super::child_file_read(&store.journal_dir, "events").unwrap();
        let flags = unsafe { libc::fcntl(input.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
        assert_eq!(
            store.inspect_recovery(&mut input).unwrap().recovery,
            super::Recovery::ClearPending
        );
        let error = super::inspect_position(&journal, &witness, None).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        let after: Vec<_> = [&events_path, &head_path, &pending_path]
            .into_iter()
            .map(|path| {
                let metadata = std::fs::metadata(path).unwrap();
                (
                    metadata.ino(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                    std::fs::read(path).unwrap(),
                )
            })
            .collect();
        assert_eq!(before, after);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_does_not_extend_a_full_journal() {
        use sha2::Digest;

        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let mut contents = std::fs::read(journal.join("events")).unwrap();
        let install: [u8; 16] = contents[8..24].try_into().unwrap();
        let mut tip: [u8; 32] = super::Sha256::digest(&contents).into();
        let make_record = |sequence: u64, previous: &[u8; 32]| {
            let mut record = Vec::with_capacity(super::RECORD_SIZE);
            record.extend_from_slice(&sequence.to_be_bytes());
            record.extend_from_slice(&[0; 32]);
            record.extend_from_slice(previous);
            let mut digest = super::Sha256::new();
            digest.update(b"APGER001");
            digest.update(install);
            digest.update(&record);
            let next: [u8; 32] = digest.finalize().into();
            record.extend_from_slice(&next);
            (record, next)
        };
        for sequence in 1..=super::MAX_RECORDS as u64 {
            let (record, next) = make_record(sequence, &tip);
            contents.extend_from_slice(&record);
            tip = next;
        }
        std::fs::write(journal.join("events"), &contents).unwrap();
        let head = super::witness_bytes(&install, super::MAX_RECORDS as u64, &tip);
        std::fs::write(witness.join("head"), &head).unwrap();
        let (record, next) = make_record(super::MAX_RECORDS as u64 + 1, &tip);
        let mut pending = super::witness_bytes(&install, super::MAX_RECORDS as u64 + 1, &next);
        pending.extend_from_slice(&record);
        std::fs::write(witness.join("pending"), &pending).unwrap();
        std::fs::set_permissions(
            witness.join("pending"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let mut events = super::child_file(&store.journal_dir, "events", false).unwrap();
        assert!(store.verified(&mut events).is_err());
        assert_eq!(std::fs::read(journal.join("events")).unwrap(), contents);
        assert_eq!(std::fs::read(witness.join("head")).unwrap(), head);
        assert_eq!(std::fs::read(witness.join("pending")).unwrap(), pending);
        std::fs::remove_file(witness.join("pending")).unwrap();
        assert_eq!(
            store.verified(&mut events).unwrap().records.len(),
            super::MAX_RECORDS
        );
        events
            .set_len((contents.len() + super::RECORD_SIZE) as u64)
            .unwrap();
        let oversized = events.metadata().unwrap().len();
        assert!(store.verified(&mut events).is_err());
        assert_eq!(events.metadata().unwrap().len(), oversized);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_inspection_is_read_only_for_valid_and_damaged_pending_states() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let old_events = std::fs::read(journal.join("events")).unwrap();
        let old_head = std::fs::read(witness.join("head")).unwrap();
        store.append(Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let new_events = std::fs::read(journal.join("events")).unwrap();
        let new_head = std::fs::read(witness.join("head")).unwrap();
        let record: [u8; super::RECORD_SIZE] = new_events[old_events.len()..].try_into().unwrap();
        let mut pending = new_head.clone();
        pending.extend_from_slice(&record);
        std::fs::write(witness.join("pending"), &pending).unwrap();
        std::fs::set_permissions(
            witness.join("pending"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let check = |expected: super::Recovery| {
            let before = (
                std::fs::read(journal.join("events")).unwrap(),
                std::fs::read(witness.join("head")).unwrap(),
                std::fs::read(witness.join("pending")).unwrap(),
            );
            let mut events = super::child_file(&store.journal_dir, "events", false).unwrap();
            assert_eq!(
                store.inspect_recovery(&mut events).unwrap().recovery,
                expected
            );
            assert_eq!(
                before,
                (
                    std::fs::read(journal.join("events")).unwrap(),
                    std::fs::read(witness.join("head")).unwrap(),
                    std::fs::read(witness.join("pending")).unwrap(),
                )
            );
        };
        for remainder in [0, 1, 31, super::RECORD_SIZE - 1] {
            let mut partial = old_events.clone();
            partial.extend_from_slice(&record[..remainder]);
            std::fs::write(journal.join("events"), partial).unwrap();
            std::fs::write(witness.join("head"), &old_head).unwrap();
            check(super::Recovery::CompleteRecord { record, remainder });
        }
        std::fs::write(journal.join("events"), &new_events).unwrap();
        check(super::Recovery::AdvanceHead);
        std::fs::write(witness.join("head"), &new_head).unwrap();
        check(super::Recovery::ClearPending);
        let mut partial = old_events.clone();
        partial.extend_from_slice(&record[..31]);
        *partial.last_mut().unwrap() ^= 1;
        std::fs::write(journal.join("events"), &partial).unwrap();
        std::fs::write(witness.join("head"), &old_head).unwrap();
        let mut events = super::child_file(&store.journal_dir, "events", false).unwrap();
        assert!(store.inspect_recovery(&mut events).is_err());
        assert_eq!(std::fs::read(journal.join("events")).unwrap(), partial);
        assert_eq!(std::fs::read(witness.join("head")).unwrap(), old_head);
        assert_eq!(std::fs::read(witness.join("pending")).unwrap(), pending);
        std::fs::write(journal.join("events"), &new_events).unwrap();
        std::fs::write(witness.join("head"), &new_head).unwrap();
        let mut damaged = pending.clone();
        damaged[super::WITNESS_SIZE + 8] ^= 1;
        std::fs::write(witness.join("pending"), &damaged).unwrap();
        let mut events = super::child_file(&store.journal_dir, "events", false).unwrap();
        assert!(store.inspect_recovery(&mut events).is_err());
        assert_eq!(std::fs::read(witness.join("pending")).unwrap(), damaged);
        assert_eq!(std::fs::read(journal.join("events")).unwrap(), new_events);
        assert_eq!(std::fs::read(witness.join("head")).unwrap(), new_head);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn witnessed_pending_record_recovers_after_interrupted_update() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let old_events = std::fs::read(journal.join("events")).unwrap();
        let old_head = std::fs::read(witness.join("head")).unwrap();
        let entry = (Uuid::new_v4(), Uuid::new_v4());
        store.append(entry.0, entry.1).unwrap();
        let new_events = std::fs::read(journal.join("events")).unwrap();
        let new_head = std::fs::read(witness.join("head")).unwrap();
        let mut pending = new_head.clone();
        pending.extend_from_slice(&new_events[old_events.len()..]);
        std::fs::write(witness.join("head"), &old_head).unwrap();
        std::fs::write(witness.join("pending"), &pending).unwrap();
        std::fs::set_permissions(
            witness.join("pending"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert_eq!(
            ErasureJournal::open(&journal, &witness)
                .unwrap()
                .verified_records()
                .unwrap(),
            vec![entry]
        );
        assert!(!witness.join("pending").exists());
        std::fs::write(journal.join("events"), &old_events).unwrap();
        std::fs::write(witness.join("head"), &old_head).unwrap();
        std::fs::write(witness.join("pending"), &pending).unwrap();
        std::fs::set_permissions(
            witness.join("pending"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert_eq!(
            ErasureJournal::open(&journal, &witness)
                .unwrap()
                .verified_records()
                .unwrap(),
            vec![entry]
        );
        std::fs::write(journal.join("events"), &new_events[..old_events.len() + 31]).unwrap();
        std::fs::write(witness.join("head"), &old_head).unwrap();
        std::fs::write(witness.join("pending"), &pending).unwrap();
        std::fs::set_permissions(
            witness.join("pending"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert_eq!(
            ErasureJournal::open(&journal, &witness)
                .unwrap()
                .verified_records()
                .unwrap(),
            vec![entry]
        );
        std::fs::write(journal.join("events"), &new_events).unwrap();
        std::fs::write(witness.join("head"), &new_head).unwrap();
        std::fs::write(witness.join("pending"), &pending).unwrap();
        std::fs::set_permissions(
            witness.join("pending"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert_eq!(
            ErasureJournal::open(&journal, &witness)
                .unwrap()
                .verified_records()
                .unwrap(),
            vec![entry]
        );
        assert!(!witness.join("pending").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mismatched_or_multiple_unwitnessed_records_are_rejected() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let original_head = std::fs::read(witness.join("head")).unwrap();
        store.append(Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let first_head = std::fs::read(witness.join("head")).unwrap();
        store.append(Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let latest_head = std::fs::read(witness.join("head")).unwrap();
        let events = std::fs::read(journal.join("events")).unwrap();
        let mut first_pending = first_head;
        first_pending.extend_from_slice(
            &events[super::HEADER_SIZE..super::HEADER_SIZE + super::RECORD_SIZE],
        );
        std::fs::write(witness.join("head"), &original_head).unwrap();
        std::fs::write(witness.join("pending"), &first_pending).unwrap();
        std::fs::set_permissions(
            witness.join("pending"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(ErasureJournal::open(&journal, &witness).is_err());
        std::fs::write(witness.join("head"), &latest_head).unwrap();
        let mut corrupted = latest_head;
        corrupted.extend_from_slice(&events[super::HEADER_SIZE + super::RECORD_SIZE..]);
        corrupted[40] ^= 1;
        std::fs::write(witness.join("pending"), corrupted).unwrap();
        assert!(ErasureJournal::open(&journal, &witness).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mismatched_partial_record_is_not_repaired() {
        let (root, journal, witness) = directories();
        let store = ErasureJournal::initialize(&journal, &witness).unwrap();
        let old_head = std::fs::read(witness.join("head")).unwrap();
        store.append(Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let events = std::fs::read(journal.join("events")).unwrap();
        let mut pending = std::fs::read(witness.join("head")).unwrap();
        pending.extend_from_slice(&events[super::HEADER_SIZE..]);
        let mut partial = events[..super::HEADER_SIZE + 31].to_vec();
        partial[super::HEADER_SIZE + 20] ^= 1;
        std::fs::write(journal.join("events"), &partial).unwrap();
        std::fs::write(witness.join("head"), old_head).unwrap();
        std::fs::write(witness.join("pending"), pending).unwrap();
        std::fs::set_permissions(
            witness.join("pending"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(ErasureJournal::open(&journal, &witness).is_err());
        assert_eq!(std::fs::read(journal.join("events")).unwrap(), partial);
        std::fs::remove_dir_all(root).unwrap();
    }
}
