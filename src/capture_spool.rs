use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::capture_crypto::{CaptureCipher, CaptureIdentity, CaptureLeg};
use crate::capture_key_usage::WrapPermit;
#[cfg(test)]
use crate::capture_key_usage::reserve_master_wrap;
use crate::capture_secret_guard::CaptureSecretGuard;
use sha2::{Digest, Sha256};
#[cfg(test)]
use sqlx::PgPool;
use uuid::Uuid;
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"APGCAP06";
const HEADER_BYTES: u64 = 158;
const RECORD_HEADER_BYTES: usize = 21;
const MANIFEST_FRAME_BYTES: u64 = 113;
const MAX_BLOCKS_PER_LEG: u64 = 1_000_000;
const MAX_SEALED_BLOCK_BYTES: usize = 1024 * 1024 + 28;

#[derive(Clone, Copy)]
pub(crate) struct CaptureLimits {
    pub per_file: u64,
    pub total: u64,
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            per_file: 256 * 1024 * 1024,
            total: 4 * 1024 * 1024 * 1024,
        }
    }
}

#[cfg(test)]
impl CaptureLimits {
    pub(crate) fn for_test(per_file: u64) -> Self {
        Self {
            per_file,
            total: per_file * 16,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CaptureSpoolError {
    #[error("capture storage failed")]
    Unavailable,
}

pub(crate) struct CaptureSpool {
    path: PathBuf,
    file: std::fs::File,
    cipher: CaptureCipher,
    limits: CaptureLimits,
    written: u64,
    next: [u64; 4],
    failed: bool,
    digest: Sha256,
    started: Instant,
    secret_guard: CaptureSecretGuard,
}

pub(crate) struct VerifiedSpool {
    pub cipher: CaptureCipher,
    pub blocks: Vec<(CaptureLeg, u64, Vec<u8>)>,
    pub elapsed_ns: Vec<u64>,
    pub complete: bool,
}

pub(crate) struct LockedSpoolFile {
    file: std::fs::File,
    path: PathBuf,
    length: u64,
}

fn acquire_capacity(
    root: &Path,
    total: u64,
    additional: u64,
) -> Result<std::fs::File, CaptureSpoolError> {
    let lock = lock_capacity(root, total)?;
    let mut used = additional;
    for entry in std::fs::read_dir(root).map_err(|_| CaptureSpoolError::Unavailable)? {
        let entry = entry.map_err(|_| CaptureSpoolError::Unavailable)?;
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "capture")
        {
            if !entry
                .file_type()
                .map_err(|_| CaptureSpoolError::Unavailable)?
                .is_file()
            {
                return Err(CaptureSpoolError::Unavailable);
            }
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(entry.path())
                .map_err(|_| CaptureSpoolError::Unavailable)?;
            let length = file
                .metadata()
                .map_err(|_| CaptureSpoolError::Unavailable)?
                .len();
            let mut trailer = [0u8; RECORD_HEADER_BYTES];
            let ended = if length >= HEADER_BYTES + MANIFEST_FRAME_BYTES {
                file.seek(SeekFrom::End(-(MANIFEST_FRAME_BYTES as i64)))
                    .and_then(|_| file.read_exact(&mut trailer))
                    .map_err(|_| CaptureSpoolError::Unavailable)?;
                trailer[0] == CaptureLeg::Manifest.index() as u8
                    && trailer[1..9] == [0; 8]
                    && trailer[9..13] == 92u32.to_be_bytes()
                    && trailer[13..21] == [0; 8]
            } else {
                false
            };
            used = used
                .checked_add(length)
                .and_then(|value| value.checked_add(if ended { 0 } else { MANIFEST_FRAME_BYTES }))
                .ok_or(CaptureSpoolError::Unavailable)?;
        }
    }
    if used > total {
        return Err(CaptureSpoolError::Unavailable);
    }
    Ok(lock)
}

fn lock_capacity(root: &Path, total: u64) -> Result<std::fs::File, CaptureSpoolError> {
    let mut lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root.join(".capture-capacity.lock"))
        .map_err(|_| CaptureSpoolError::Unavailable)?;
    lock.lock().map_err(|_| CaptureSpoolError::Unavailable)?;
    let metadata = lock
        .metadata()
        .map_err(|_| CaptureSpoolError::Unavailable)?;
    if !metadata.is_file() {
        return Err(CaptureSpoolError::Unavailable);
    }
    if metadata.len() == 0 {
        lock.write_all(&total.to_be_bytes())
            .and_then(|_| lock.sync_all())
            .and_then(|_| std::fs::File::open(root)?.sync_all())
            .map_err(|_| CaptureSpoolError::Unavailable)?;
    } else {
        if metadata.len() != 8 {
            return Err(CaptureSpoolError::Unavailable);
        }
        let mut saved = [0u8; 8];
        lock.seek(SeekFrom::Start(0))
            .and_then(|_| lock.read_exact(&mut saved))
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        if u64::from_be_bytes(saved) != total {
            return Err(CaptureSpoolError::Unavailable);
        }
    }
    Ok(lock)
}

impl CaptureSpool {
    #[cfg(test)]
    pub async fn create(
        pool: &PgPool,
        root: &Path,
        identity: CaptureIdentity,
        master: &[u8; 32],
        secrets: &[&[u8]],
        limits: CaptureLimits,
    ) -> Result<Self, CaptureSpoolError> {
        let permit = reserve_master_wrap(pool, master)
            .await
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        Self::create_with_permit(root, identity, master, secrets, permit, limits)
    }

    pub(crate) fn create_with_permit(
        root: &Path,
        identity: CaptureIdentity,
        master: &[u8; 32],
        secrets: &[&[u8]],
        permit: WrapPermit,
        limits: CaptureLimits,
    ) -> Result<Self, CaptureSpoolError> {
        if limits.per_file < HEADER_BYTES + MANIFEST_FRAME_BYTES || limits.total < limits.per_file {
            return Err(CaptureSpoolError::Unavailable);
        }
        if identity.created_ms <= 0 || !(1..=365).contains(&identity.retention_days) {
            return Err(CaptureSpoolError::Unavailable);
        }
        let _quota = acquire_capacity(root, limits.total, HEADER_BYTES + MANIFEST_FRAME_BYTES)?;
        let secret_guard =
            CaptureSecretGuard::new(secrets).map_err(|_| CaptureSpoolError::Unavailable)?;
        let cipher = CaptureCipher::create(master, identity, permit)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let path = root.join(format!("{}.capture", identity.capture));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        file.lock().map_err(|_| CaptureSpoolError::Unavailable)?;
        file.write_all(MAGIC)
            .and_then(|_| file.write_all(identity.tenant.as_bytes()))
            .and_then(|_| file.write_all(identity.capture.as_bytes()))
            .and_then(|_| file.write_all(identity.user.as_bytes()))
            .and_then(|_| file.write_all(&identity.created_ms.to_be_bytes()))
            .and_then(|_| file.write_all(&identity.retention_days.to_be_bytes()))
            .and_then(|_| file.write_all(cipher.fingerprint()))
            .and_then(|_| file.write_all(cipher.wrapped_key()))
            .and_then(|_| file.sync_all())
            .and_then(|_| std::fs::File::open(root)?.sync_all())
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        Ok(Self {
            path,
            file,
            cipher,
            limits,
            written: HEADER_BYTES,
            next: [0; 4],
            failed: false,
            digest: Sha256::new(),
            started: Instant::now(),
            secret_guard,
        })
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(
        &mut self,
        leg: CaptureLeg,
        sequence: u64,
        plaintext: &[u8],
    ) -> Result<(), CaptureSpoolError> {
        let index = leg.index();
        if index >= self.next.len()
            || self.failed
            || sequence != self.next[index]
            || sequence >= MAX_BLOCKS_PER_LEG
        {
            return Err(CaptureSpoolError::Unavailable);
        }
        let elapsed = u64::try_from(self.started.elapsed().as_nanos())
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        if elapsed > i64::MAX as u64 {
            return Err(CaptureSpoolError::Unavailable);
        }
        let position = self.next.iter().sum();
        let sealed = self
            .cipher
            .seal_timed(leg, sequence, position, elapsed, plaintext)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let length = u32::try_from(sealed.len()).map_err(|_| CaptureSpoolError::Unavailable)?;
        let total = u64::try_from(RECORD_HEADER_BYTES + sealed.len())
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let written = self
            .written
            .checked_add(total)
            .filter(|value| {
                value
                    .checked_add(MANIFEST_FRAME_BYTES)
                    .is_some_and(|reserved| reserved <= self.limits.per_file)
            })
            .ok_or(CaptureSpoolError::Unavailable)?;
        let mut header = [0u8; RECORD_HEADER_BYTES];
        if self.secret_guard.check(index, plaintext).is_err() {
            self.failed = true;
            return Err(CaptureSpoolError::Unavailable);
        }
        header[0] = index as u8;
        header[1..9].copy_from_slice(&sequence.to_be_bytes());
        header[9..13].copy_from_slice(&length.to_be_bytes());
        header[13..21].copy_from_slice(&elapsed.to_be_bytes());
        let root = self.path.parent().ok_or(CaptureSpoolError::Unavailable)?;
        let _quota = acquire_capacity(root, self.limits.total, total)?;
        if self
            .file
            .write_all(&header)
            .and_then(|_| self.file.write_all(&sealed))
            .and_then(|_| self.file.sync_all())
            .is_err()
        {
            self.failed = true;
            return Err(CaptureSpoolError::Unavailable);
        }
        self.written = written;
        self.next[index] += 1;
        self.digest.update(header);
        self.digest.update(sealed);
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), CaptureSpoolError> {
        if self.failed {
            return Err(CaptureSpoolError::Unavailable);
        }
        let mut manifest = self.digest.clone().finalize().to_vec();
        for value in self.next {
            manifest.extend_from_slice(&value.to_be_bytes());
        }
        let sealed = self
            .cipher
            .seal(CaptureLeg::Manifest, 0, &manifest)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let length = u32::try_from(sealed.len()).map_err(|_| CaptureSpoolError::Unavailable)?;
        let total = u64::try_from(RECORD_HEADER_BYTES + sealed.len())
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        self.written
            .checked_add(total)
            .filter(|value| *value <= self.limits.per_file)
            .ok_or(CaptureSpoolError::Unavailable)?;
        let mut header = [0u8; RECORD_HEADER_BYTES];
        header[0] = CaptureLeg::Manifest.index() as u8;
        header[9..13].copy_from_slice(&length.to_be_bytes());
        let root = self.path.parent().ok_or(CaptureSpoolError::Unavailable)?;
        let _quota = acquire_capacity(root, self.limits.total, 0)?;
        self.file
            .write_all(&header)
            .and_then(|_| self.file.write_all(&sealed))
            .and_then(|_| self.file.sync_all())
            .map_err(|_| CaptureSpoolError::Unavailable)
    }

    #[cfg(test)]
    pub fn read(
        path: &Path,
        identity: CaptureIdentity,
        master: &[u8; 32],
        max_bytes: u64,
    ) -> Result<Vec<Vec<u8>>, CaptureSpoolError> {
        let verified = Self::read_verified(path, identity, master, max_bytes)?;
        verified
            .blocks
            .into_iter()
            .enumerate()
            .filter(|(_, (leg, _, _))| leg.index() != CaptureLeg::Manifest.index())
            .map(|(position, (leg, sequence, sealed))| {
                verified
                    .cipher
                    .unseal_timed(
                        leg,
                        sequence,
                        position as u64,
                        verified.elapsed_ns[position],
                        &sealed,
                    )
                    .map_err(|_| CaptureSpoolError::Unavailable)
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn read_verified(
        path: &Path,
        identity: CaptureIdentity,
        master: &[u8; 32],
        max_bytes: u64,
    ) -> Result<VerifiedSpool, CaptureSpoolError> {
        let file = std::fs::File::open(path).map_err(|_| CaptureSpoolError::Unavailable)?;
        let size = file
            .metadata()
            .map_err(|_| CaptureSpoolError::Unavailable)?
            .len();
        Self::verify_reader(file, size, identity, master, max_bytes, false, true)
    }

    #[cfg(test)]
    pub(crate) fn recover(
        path: &Path,
        master: &[u8; 32],
        max_bytes: u64,
    ) -> Result<VerifiedSpool, CaptureSpoolError> {
        Self::recover_inner(path, master, max_bytes, false)
    }

    #[cfg(test)]
    pub(crate) fn recover_incomplete(
        path: &Path,
        master: &[u8; 32],
        max_bytes: u64,
    ) -> Result<VerifiedSpool, CaptureSpoolError> {
        Self::recover_inner(path, master, max_bytes, true)
    }

    fn recover_inner(
        path: &Path,
        master: &[u8; 32],
        max_bytes: u64,
        allow_incomplete: bool,
    ) -> Result<VerifiedSpool, CaptureSpoolError> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        file.try_lock_shared()
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        Self::verify_open_file(path, &mut file, master, max_bytes, allow_incomplete)
    }

    fn verify_open_file(
        path: &Path,
        file: &mut std::fs::File,
        master: &[u8; 32],
        max_bytes: u64,
        allow_incomplete: bool,
    ) -> Result<VerifiedSpool, CaptureSpoolError> {
        let metadata = file
            .metadata()
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        if !metadata.is_file() || metadata.len() < HEADER_BYTES || metadata.len() > max_bytes {
            return Err(CaptureSpoolError::Unavailable);
        }
        let mut header = [0u8; 98];
        file.read_exact(&mut header)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        if &header[..8] != MAGIC {
            return Err(CaptureSpoolError::Unavailable);
        }
        let identity = CaptureIdentity {
            tenant: Uuid::from_slice(&header[8..24]).map_err(|_| CaptureSpoolError::Unavailable)?,
            capture: Uuid::from_slice(&header[24..40])
                .map_err(|_| CaptureSpoolError::Unavailable)?,
            user: Uuid::from_slice(&header[40..56]).map_err(|_| CaptureSpoolError::Unavailable)?,
            created_ms: i64::from_be_bytes(
                header[56..64]
                    .try_into()
                    .map_err(|_| CaptureSpoolError::Unavailable)?,
            ),
            retention_days: u16::from_be_bytes(
                header[64..66]
                    .try_into()
                    .map_err(|_| CaptureSpoolError::Unavailable)?,
            ),
        };
        if path.file_name().and_then(|name| name.to_str())
            != Some(format!("{}.capture", identity.capture).as_str())
        {
            return Err(CaptureSpoolError::Unavailable);
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        Self::verify_reader(
            file,
            metadata.len(),
            identity,
            master,
            max_bytes,
            allow_incomplete,
            true,
        )
    }

    pub(crate) fn open_complete_for_cleanup(
        path: &Path,
        key_root: &Path,
        max_bytes: u64,
    ) -> Result<(VerifiedSpool, LockedSpoolFile), CaptureSpoolError> {
        Self::open_for_cleanup(path, key_root, max_bytes, false)
    }

    pub(crate) fn open_deleted_for_cleanup(
        path: &Path,
        key_root: &Path,
        max_bytes: u64,
    ) -> Result<(VerifiedSpool, LockedSpoolFile), CaptureSpoolError> {
        Self::open_for_cleanup(path, key_root, max_bytes, true)
    }

    pub(crate) fn is_truncated_for_cleanup(path: &Path) -> Result<bool, CaptureSpoolError> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        file.try_lock()
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let metadata = file
            .metadata()
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        if !metadata.is_file() {
            return Err(CaptureSpoolError::Unavailable);
        }
        Ok(metadata.len() < HEADER_BYTES)
    }

    fn open_for_cleanup(
        path: &Path,
        key_root: &Path,
        max_bytes: u64,
        allow_incomplete: bool,
    ) -> Result<(VerifiedSpool, LockedSpoolFile), CaptureSpoolError> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        file.try_lock()
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let mut header = [0u8; 98];
        file.read_exact(&mut header)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        if &header[..8] != MAGIC {
            return Err(CaptureSpoolError::Unavailable);
        }
        let fingerprint: [u8; 32] = header[66..98]
            .try_into()
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let master = crate::capture_master_key::load_versioned_key(key_root, &fingerprint)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let verified =
            Self::verify_open_file(path, &mut file, &master, max_bytes, allow_incomplete)?;
        let length = file
            .metadata()
            .map_err(|_| CaptureSpoolError::Unavailable)?
            .len();
        Ok((
            verified,
            LockedSpoolFile {
                file,
                path: path.to_owned(),
                length,
            },
        ))
    }

    pub(crate) fn remove_locked(
        locked: LockedSpoolFile,
        total: u64,
    ) -> Result<(), CaptureSpoolError> {
        use std::os::unix::fs::MetadataExt;
        let root = locked.path.parent().ok_or(CaptureSpoolError::Unavailable)?;
        let _quota = lock_capacity(root, total)?;
        let opened = locked
            .file
            .metadata()
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let current =
            std::fs::symlink_metadata(&locked.path).map_err(|_| CaptureSpoolError::Unavailable)?;
        if !current.is_file()
            || opened.dev() != current.dev()
            || opened.ino() != current.ino()
            || opened.len() != locked.length
        {
            return Err(CaptureSpoolError::Unavailable);
        }
        std::fs::remove_file(&locked.path)
            .and_then(|_| std::fs::File::open(root)?.sync_all())
            .map_err(|_| CaptureSpoolError::Unavailable)
    }

    pub(crate) fn recover_with_keyring(
        path: &Path,
        key_root: &Path,
        max_bytes: u64,
    ) -> Result<VerifiedSpool, CaptureSpoolError> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let metadata = file
            .metadata()
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        if !metadata.is_file() || metadata.len() < HEADER_BYTES || metadata.len() > max_bytes {
            return Err(CaptureSpoolError::Unavailable);
        }
        let mut header = [0u8; 98];
        file.read_exact(&mut header)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        if &header[..8] != MAGIC {
            return Err(CaptureSpoolError::Unavailable);
        }
        let fingerprint: [u8; 32] = header[66..98]
            .try_into()
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let master = crate::capture_master_key::load_versioned_key(key_root, &fingerprint)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        Self::recover_inner(path, &master, max_bytes, true)
    }

    #[cfg(test)]
    pub(crate) fn verify_serialized(
        data: &[u8],
        identity: CaptureIdentity,
        master: &[u8; 32],
        max_bytes: u64,
        allow_incomplete: bool,
    ) -> Result<VerifiedSpool, CaptureSpoolError> {
        Self::verify_reader(
            data,
            data.len() as u64,
            identity,
            master,
            max_bytes,
            allow_incomplete,
            true,
        )
    }

    pub(crate) fn verify_without_retaining(
        file: impl Read,
        size: u64,
        identity: CaptureIdentity,
        master: &[u8; 32],
        max_bytes: u64,
        allow_incomplete: bool,
    ) -> Result<VerifiedSpool, CaptureSpoolError> {
        Self::verify_reader(
            file,
            size,
            identity,
            master,
            max_bytes,
            allow_incomplete,
            false,
        )
    }

    fn verify_reader(
        mut file: impl Read,
        size: u64,
        identity: CaptureIdentity,
        master: &[u8; 32],
        max_bytes: u64,
        allow_incomplete: bool,
        retain_blocks: bool,
    ) -> Result<VerifiedSpool, CaptureSpoolError> {
        if size < HEADER_BYTES || size > max_bytes {
            return Err(CaptureSpoolError::Unavailable);
        }
        let mut header = [0u8; HEADER_BYTES as usize];
        file.read_exact(&mut header)
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        if &header[..8] != MAGIC
            || &header[8..24] != identity.tenant.as_bytes()
            || &header[24..40] != identity.capture.as_bytes()
            || &header[40..56] != identity.user.as_bytes()
            || header[56..64] != identity.created_ms.to_be_bytes()
            || header[64..66] != identity.retention_days.to_be_bytes()
            || identity.created_ms <= 0
            || !(1..=365).contains(&identity.retention_days)
        {
            return Err(CaptureSpoolError::Unavailable);
        }
        if &header[66..98] != sha2::Sha256::digest(master).as_slice() {
            return Err(CaptureSpoolError::Unavailable);
        }
        let cipher = CaptureCipher::open(master, identity, &header[98..])
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let mut remaining = size - HEADER_BYTES;
        let mut next = [0u64; 4];
        let mut digest = Sha256::new();
        let mut records = Vec::new();
        let mut elapsed_ns = Vec::new();
        let mut last_elapsed = None;
        let mut position = 0u64;
        while remaining > 0 {
            if remaining < RECORD_HEADER_BYTES as u64 {
                if allow_incomplete {
                    break;
                }
                return Err(CaptureSpoolError::Unavailable);
            }
            let mut record = [0u8; RECORD_HEADER_BYTES];
            file.read_exact(&mut record)
                .map_err(|_| CaptureSpoolError::Unavailable)?;
            let leg =
                CaptureLeg::from_index(record[0]).map_err(|_| CaptureSpoolError::Unavailable)?;
            let sequence = u64::from_be_bytes(
                record[1..9]
                    .try_into()
                    .map_err(|_| CaptureSpoolError::Unavailable)?,
            );
            let length = u32::from_be_bytes(
                record[9..13]
                    .try_into()
                    .map_err(|_| CaptureSpoolError::Unavailable)?,
            ) as usize;
            let elapsed = u64::from_be_bytes(
                record[13..21]
                    .try_into()
                    .map_err(|_| CaptureSpoolError::Unavailable)?,
            );
            remaining -= RECORD_HEADER_BYTES as u64;
            if length > MAX_SEALED_BLOCK_BYTES {
                return Err(CaptureSpoolError::Unavailable);
            }
            if length as u64 > remaining {
                if allow_incomplete {
                    break;
                }
                return Err(CaptureSpoolError::Unavailable);
            }
            let mut sealed = vec![0; length];
            file.read_exact(&mut sealed)
                .map_err(|_| CaptureSpoolError::Unavailable)?;
            remaining -= length as u64;
            if leg.index() == CaptureLeg::Manifest.index() {
                if sequence != 0 || remaining != 0 || elapsed != 0 {
                    return Err(CaptureSpoolError::Unavailable);
                }
                let manifest = Zeroizing::new(
                    cipher
                        .unseal(leg, sequence, &sealed)
                        .map_err(|_| CaptureSpoolError::Unavailable)?,
                );
                let mut expected = digest.finalize().to_vec();
                for value in next {
                    expected.extend_from_slice(&value.to_be_bytes());
                }
                if *manifest != expected {
                    return Err(CaptureSpoolError::Unavailable);
                }
                if retain_blocks {
                    records.push((leg, sequence, sealed));
                    elapsed_ns.push(0);
                }
                return Ok(VerifiedSpool {
                    cipher,
                    blocks: records,
                    elapsed_ns,
                    complete: true,
                });
            }
            if sequence != next[leg.index()] || sequence >= MAX_BLOCKS_PER_LEG {
                return Err(CaptureSpoolError::Unavailable);
            }
            if elapsed > i64::MAX as u64 || last_elapsed.is_some_and(|last| elapsed < last) {
                return Err(CaptureSpoolError::Unavailable);
            }
            digest.update(record);
            digest.update(&sealed);
            let _plaintext = Zeroizing::new(
                cipher
                    .unseal_timed(leg, sequence, position, elapsed, &sealed)
                    .map_err(|_| CaptureSpoolError::Unavailable)?,
            );
            if retain_blocks {
                records.push((leg, sequence, sealed));
                elapsed_ns.push(elapsed);
            }
            last_elapsed = Some(elapsed);
            position += 1;
            next[leg.index()] += 1;
        }
        if allow_incomplete {
            Ok(VerifiedSpool {
                cipher,
                blocks: records,
                elapsed_ns,
                complete: false,
            })
        } else {
            Err(CaptureSpoolError::Unavailable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CaptureLimits, CaptureSpool};
    use crate::capture_crypto::{CaptureIdentity, CaptureLeg};
    use uuid::Uuid;

    #[test]
    #[ignore]
    fn backup_restore_fixture() {
        let root = std::path::PathBuf::from(std::env::var_os("CAPTURE_FIXTURE_ROOT").unwrap());
        let spool = root.join("spool");
        let keys = root.join("keys");
        let capture = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        if std::env::var_os("CAPTURE_FIXTURE_CREATE").is_some() {
            std::fs::create_dir_all(&spool).unwrap();
            let master = *b"synthetic-private-key-32-bytes!!";
            let identity = CaptureIdentity {
                tenant: Uuid::nil(),
                capture,
                user: Uuid::nil(),
                created_ms: chrono::Utc::now().timestamp_millis(),
                retention_days: 30,
            };
            let mut item = CaptureSpool::create_with_permit(
                &spool,
                identity,
                &master,
                &[],
                crate::capture_key_usage::WrapPermit::for_test(&master),
                CaptureLimits::for_test(4096),
            )
            .unwrap();
            item.append(CaptureLeg::ClientRequest, 0, b"synthetic-capture")
                .unwrap();
            item.finish().unwrap();
        } else {
            let item = CaptureSpool::recover_with_keyring(
                &spool.join(format!("{capture}.capture")),
                &keys,
                4096,
            )
            .unwrap();
            assert!(item.complete);
            assert_eq!(
                item.cipher
                    .unseal_timed(
                        item.blocks[0].0,
                        item.blocks[0].1,
                        0,
                        item.elapsed_ns[0],
                        &item.blocks[0].2
                    )
                    .unwrap(),
                b"synthetic-capture"
            );
        }
    }

    #[test]
    fn capacity_is_shared_and_conflicting_limits_are_rejected() {
        let root = std::env::temp_dir().join(format!("capture-capacity-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let second = CaptureIdentity {
            tenant: identity.tenant,
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let master = [9u8; 32];
        let limits = CaptureLimits {
            per_file: 400,
            total: 550,
        };
        let mut first = CaptureSpool::create_with_permit(
            &root,
            identity,
            &master,
            &[],
            crate::capture_key_usage::WrapPermit::for_test(&master),
            limits,
        )
        .unwrap();
        first
            .append(CaptureLeg::ClientRequest, 0, b"some bytes")
            .unwrap();
        assert!(
            CaptureSpool::create_with_permit(
                &root,
                second,
                &master,
                &[],
                crate::capture_key_usage::WrapPermit::for_test(&master),
                limits
            )
            .is_err()
        );
        assert!(!root.join(format!("{}.capture", second.capture)).exists());
        assert!(
            CaptureSpool::create_with_permit(
                &root,
                second,
                &master,
                &[],
                crate::capture_key_usage::WrapPermit::for_test(&master),
                CaptureLimits {
                    per_file: 400,
                    total: 600
                }
            )
            .is_err()
        );
        first.finish().unwrap();
    }

    #[test]
    fn completed_files_release_only_their_reserved_footer_space() {
        let root = std::env::temp_dir().join(format!("capture-reserved-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let one = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let two = CaptureIdentity {
            tenant: one.tenant,
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let master = [10u8; 32];
        let limits = CaptureLimits {
            per_file: 400,
            total: 650,
        };
        let mut first = CaptureSpool::create_with_permit(
            &root,
            one,
            &master,
            &[],
            crate::capture_key_usage::WrapPermit::for_test(&master),
            limits,
        )
        .unwrap();
        first
            .append(CaptureLeg::ClientRequest, 0, b"some bytes")
            .unwrap();
        let second = CaptureSpool::create_with_permit(
            &root,
            two,
            &master,
            &[],
            crate::capture_key_usage::WrapPermit::for_test(&master),
            limits,
        )
        .unwrap();
        first.finish().unwrap();
        second.finish().unwrap();
        assert!(
            CaptureSpool::read(
                &root.join(format!("{}.capture", one.capture)),
                one,
                &master,
                400
            )
            .is_ok()
        );
        assert!(
            CaptureSpool::read(
                &root.join(format!("{}.capture", two.capture)),
                two,
                &master,
                400
            )
            .is_ok()
        );
        let third = CaptureIdentity {
            tenant: one.tenant,
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        assert!(
            CaptureSpool::create_with_permit(
                &root,
                third,
                &master,
                &[],
                crate::capture_key_usage::WrapPermit::for_test(&master),
                limits
            )
            .is_err()
        );
    }

    #[test]
    fn torn_tail_recovers_only_authenticated_complete_blocks() {
        let root = std::env::temp_dir().join(format!("capture-torn-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let master = [11u8; 32];
        let mut spool = CaptureSpool::create_with_permit(
            &root,
            identity,
            &master,
            &[],
            crate::capture_key_usage::WrapPermit::for_test(&master),
            CaptureLimits::for_test(4096),
        )
        .unwrap();
        spool
            .append(CaptureLeg::ClientRequest, 0, b"first")
            .unwrap();
        let first_end = std::fs::metadata(spool.path()).unwrap().len() as usize;
        spool
            .append(CaptureLeg::ProviderResponse, 0, b"second")
            .unwrap();
        let path = spool.path().to_owned();
        spool.finish().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        let partial = CaptureSpool::recover_incomplete(&path, &master, 4096).unwrap();
        assert!(!partial.complete);
        assert_eq!(partial.blocks.len(), 2);
        assert!(CaptureSpool::recover(&path, &master, 4096).is_err());
        std::fs::write(&path, &bytes[..first_end + 25]).unwrap();
        let partial = CaptureSpool::recover_incomplete(&path, &master, 4096).unwrap();
        assert!(!partial.complete);
        assert_eq!(partial.blocks.len(), 1);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len() as usize,
            first_end + 25
        );
        let mut corrupted = bytes[..first_end + 25].to_vec();
        corrupted[super::HEADER_BYTES as usize + super::RECORD_HEADER_BYTES + 12] ^= 1;
        std::fs::write(&path, &corrupted).unwrap();
        assert!(CaptureSpool::recover_incomplete(&path, &master, 4096).is_err());
    }

    #[test]
    fn separate_process_holds_the_capacity_lock() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let root = std::env::temp_dir().join(format!("capture-capacity-lock-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let lock_path = root.join(".capture-capacity.lock");
        std::fs::write(&lock_path, 400u64.to_be_bytes()).unwrap();
        let mut child = Command::new("flock")
            .arg("-x")
            .arg(&lock_path)
            .arg("sleep")
            .arg("1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            let probe = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .unwrap();
            if probe.try_lock().is_err() {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker_root = root.clone();
        let worker = std::thread::spawn(move || {
            let lock = super::acquire_capacity(&worker_root, 400, 0);
            sender.send(lock.is_ok()).unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(child.wait().unwrap().success());
        assert!(receiver.recv_timeout(Duration::from_secs(2)).unwrap());
        worker.join().unwrap();
    }

    #[test]
    fn credential_split_across_blocks_stops_the_capture() {
        let root = std::env::temp_dir().join(format!("capture-secret-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let master = [8u8; 32];
        let mut spool = CaptureSpool::create_with_permit(
            &root,
            identity,
            &master,
            &[b"provider-token"],
            crate::capture_key_usage::WrapPermit::for_test(&master),
            super::CaptureLimits::for_test(4096),
        )
        .unwrap();
        spool
            .append(CaptureLeg::ClientRequest, 0, b"provider-")
            .unwrap();
        let path = spool.path().to_owned();
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(
            spool
                .append(CaptureLeg::ClientRequest, 1, b"token")
                .is_err()
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        assert!(
            spool
                .append(CaptureLeg::ClientResponse, 0, b"ordinary")
                .is_err()
        );
        assert!(spool.finish().is_err());
        assert!(CaptureSpool::read(&path, identity, &master, 4096).is_err());
    }

    #[test]
    fn encrypted_spool_is_durable_bounded_and_rejects_tampering() {
        let root = std::env::temp_dir().join(format!("capture-spool-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let master = [3u8; 32];
        let mut spool = CaptureSpool::create_with_permit(
            &root,
            identity,
            &master,
            &[],
            crate::capture_key_usage::WrapPermit::for_test(&master),
            super::CaptureLimits::for_test(4096),
        )
        .unwrap();
        spool
            .append(CaptureLeg::ClientRequest, 0, b"private prompt")
            .unwrap();
        spool
            .append(CaptureLeg::ProviderResponse, 0, b"private response")
            .unwrap();
        assert!(
            spool
                .append(CaptureLeg::ProviderResponse, 0, b"duplicate")
                .is_err()
        );
        assert!(
            spool
                .append(CaptureLeg::ClientResponse, 0, &vec![1; 4096])
                .is_err()
        );
        let path = spool.path().to_owned();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(CaptureSpool::read(&path, identity, &master, 4096).is_err());
        spool.finish().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let recovered = CaptureSpool::recover(&path, &master, 4096).unwrap();
        assert_eq!(recovered.cipher.identity().user, identity.user);
        let mut wrong_owner = bytes.clone();
        wrong_owner[40] ^= 1;
        std::fs::write(&path, &wrong_owner).unwrap();
        assert!(CaptureSpool::recover(&path, &master, 4096).is_err());
        let mut wrong_time = bytes.clone();
        wrong_time[63] ^= 1;
        std::fs::write(&path, &wrong_time).unwrap();
        assert!(CaptureSpool::recover(&path, &master, 4096).is_err());
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            !bytes
                .windows(b"private prompt".len())
                .any(|value| value == b"private prompt")
        );
        assert!(
            !bytes
                .windows(b"private response".len())
                .any(|value| value == b"private response")
        );
        let records = CaptureSpool::read(&path, identity, &master, 4096).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0], b"private prompt");
        assert_eq!(records[1], b"private response");
        assert!(CaptureSpool::read(&path, identity, &[4u8; 32], 4096).is_err());
        let mut tampered_time = bytes.clone();
        tampered_time[171] ^= 1;
        std::fs::write(&path, &tampered_time).unwrap();
        assert!(CaptureSpool::read(&path, identity, &master, 4096).is_err());
        std::fs::write(&path, &bytes[..bytes.len() - 113]).unwrap();
        assert!(CaptureSpool::read(&path, identity, &master, 4096).is_err());
        let mut damaged = bytes.clone();
        *damaged.last_mut().unwrap() ^= 1;
        std::fs::write(&path, damaged).unwrap();
        assert!(CaptureSpool::read(&path, identity, &master, 4096).is_err());
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(CaptureSpool::read(&path, identity, &master, 4096).is_err());
    }
}
