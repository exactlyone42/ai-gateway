#[cfg(test)]
use crate::capture_anchor::create_versioned_key;
use crate::capture_master_key::load_versioned_key;
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::io::Read;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
#[error("capture keyring unavailable")]
pub(crate) struct CaptureKeyringError;

pub(crate) fn current(root: &Path) -> Result<Zeroizing<[u8; 32]>, CaptureKeyringError> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(root.join("current"))
        .map_err(|_| CaptureKeyringError)?;
    let meta = file.metadata().map_err(|_| CaptureKeyringError)?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.len() != 32 {
        return Err(CaptureKeyringError);
    }
    let mut fingerprint = [0u8; 32];
    file.read_exact(&mut fingerprint)
        .map_err(|_| CaptureKeyringError)?;
    load_versioned_key(root, &fingerprint).map_err(|_| CaptureKeyringError)
}

pub(crate) fn locked_current(
    root: &Path,
) -> Result<(Zeroizing<[u8; 32]>, std::fs::File), CaptureKeyringError> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(root.join(".rotation.lock"))
        .map_err(|_| CaptureKeyringError)?;
    let meta = lock.metadata().map_err(|_| CaptureKeyringError)?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 {
        return Err(CaptureKeyringError);
    }
    lock.try_lock_shared().map_err(|_| CaptureKeyringError)?;
    match std::fs::symlink_metadata(root.join(".initializing")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => return Err(CaptureKeyringError),
    }
    Ok((current(root)?, lock))
}

fn complete_initial_file(path: &Path, expected: &[u8]) -> Result<(), CaptureKeyringError> {
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => options
            .create_new(true)
            .open(path)
            .map_err(|_| CaptureKeyringError)?,
        Err(_) => return Err(CaptureKeyringError),
    };
    let metadata = file.metadata().map_err(|_| CaptureKeyringError)?;
    if !metadata.is_file()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
        || metadata.len() > expected.len() as u64
    {
        return Err(CaptureKeyringError);
    }
    let mut prefix = Zeroizing::new(Vec::new());
    file.read_to_end(&mut prefix)
        .map_err(|_| CaptureKeyringError)?;
    if !expected.starts_with(&prefix) {
        return Err(CaptureKeyringError);
    }
    file.write_all(&expected[prefix.len()..])
        .and_then(|_| file.sync_all())
        .map_err(|_| CaptureKeyringError)?;
    std::fs::File::open(path.parent().ok_or(CaptureKeyringError)?)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| CaptureKeyringError)
}

fn initialization_key(root: &Path) -> Result<Zeroizing<[u8; 32]>, CaptureKeyringError> {
    let path = root.join(".initializing");
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut key = Zeroizing::new([0u8; 32]);
            getrandom::fill(key.as_mut()).map_err(|_| CaptureKeyringError)?;
            let mut intent = Zeroizing::new(b"APGKINI1".to_vec());
            intent.extend_from_slice(key.as_ref());
            let checksum = Sha256::digest(&*intent);
            intent.extend_from_slice(&checksum);
            let temporary = root.join(format!(".initializing-{}", Uuid::new_v4()));
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&temporary)
                .map_err(|_| CaptureKeyringError)?;
            output
                .write_all(&intent)
                .and_then(|_| output.sync_all())
                .map_err(|_| CaptureKeyringError)?;
            let from =
                CString::new(temporary.as_os_str().as_bytes()).map_err(|_| CaptureKeyringError)?;
            let to = CString::new(path.as_os_str().as_bytes()).map_err(|_| CaptureKeyringError)?;
            if unsafe {
                libc::renameat2(
                    libc::AT_FDCWD,
                    from.as_ptr(),
                    libc::AT_FDCWD,
                    to.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            } != 0
            {
                return Err(CaptureKeyringError);
            }
            std::fs::File::open(root)
                .and_then(|dir| dir.sync_all())
                .map_err(|_| CaptureKeyringError)?;
            return Ok(key);
        }
        Err(_) => return Err(CaptureKeyringError),
    };
    let meta = file.metadata().map_err(|_| CaptureKeyringError)?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.nlink() != 1 || meta.len() != 72 {
        return Err(CaptureKeyringError);
    }
    let mut bytes = Zeroizing::new([0u8; 72]);
    file.read_exact(bytes.as_mut())
        .map_err(|_| CaptureKeyringError)?;
    if &bytes[..8] != b"APGKINI1" || bytes[40..] != Sha256::digest(&bytes[..40])[..] {
        return Err(CaptureKeyringError);
    }
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&bytes[8..40]);
    Ok(key)
}

#[cfg(test)]
pub(crate) fn rotate(
    root: &Path,
    anchors: &Path,
    spool: &Path,
) -> Result<[u8; 32], CaptureKeyringError> {
    let _backup_guard =
        crate::capture_backup_barrier::try_shared(spool).map_err(|_| CaptureKeyringError)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root.join(".rotation.lock"))
        .map_err(|_| CaptureKeyringError)?;
    lock.lock().map_err(|_| CaptureKeyringError)?;
    if !lock.metadata().map_err(|_| CaptureKeyringError)?.is_file() {
        return Err(CaptureKeyringError);
    }
    write_new_current(root, anchors)
}

pub(crate) fn initialize(
    root: &Path,
    anchors: &Path,
    spool: &Path,
) -> Result<[u8; 32], CaptureKeyringError> {
    crate::capture_paths::validate_paths([spool, root, anchors])
        .map_err(|_| CaptureKeyringError)?;
    let _backup_guard =
        crate::capture_backup_barrier::try_shared(spool).map_err(|_| CaptureKeyringError)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(root.join(".rotation.lock"))
        .map_err(|_| CaptureKeyringError)?;
    let metadata = lock.metadata().map_err(|_| CaptureKeyringError)?;
    if !metadata.is_file() || metadata.mode() & 0o077 != 0 {
        return Err(CaptureKeyringError);
    }
    lock.try_lock().map_err(|_| CaptureKeyringError)?;
    let existing_intent = match std::fs::symlink_metadata(root.join(".initializing")) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err(CaptureKeyringError),
    };
    for entry in std::fs::read_dir(spool).map_err(|_| CaptureKeyringError)? {
        if entry.map_err(|_| CaptureKeyringError)?.file_name() != ".capture-backup.lock" {
            return Err(CaptureKeyringError);
        }
    }
    if !existing_intent {
        for (path, allowed) in [(root, Some(".rotation.lock")), (anchors, None)] {
            for entry in std::fs::read_dir(path).map_err(|_| CaptureKeyringError)? {
                let entry = entry.map_err(|_| CaptureKeyringError)?;
                if allowed.is_none_or(|name| entry.file_name() != name) {
                    return Err(CaptureKeyringError);
                }
            }
        }
    }
    let key = initialization_key(root)?;
    let fingerprint: [u8; 32] = Sha256::digest(key.as_ref()).into();
    let name: String = fingerprint
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let key_name = format!("{name}.key");
    let anchor_name = format!("{name}.anchor");
    for entry in std::fs::read_dir(root).map_err(|_| CaptureKeyringError)? {
        let name = entry.map_err(|_| CaptureKeyringError)?.file_name();
        if name != ".rotation.lock"
            && name != ".initializing"
            && name != "current"
            && name != key_name.as_str()
        {
            return Err(CaptureKeyringError);
        }
    }
    for entry in std::fs::read_dir(anchors).map_err(|_| CaptureKeyringError)? {
        if entry.map_err(|_| CaptureKeyringError)?.file_name() != anchor_name.as_str() {
            return Err(CaptureKeyringError);
        }
    }
    complete_initial_file(&root.join(key_name), key.as_ref())?;
    complete_initial_file(
        &anchors.join(anchor_name),
        &crate::capture_anchor::initial_anchor(&fingerprint),
    )?;
    complete_initial_file(&root.join("current"), &fingerprint)?;
    std::fs::remove_file(root.join(".initializing"))
        .and_then(|_| std::fs::File::open(root)?.sync_all())
        .map_err(|_| CaptureKeyringError)?;
    lock.unlock().map_err(|_| CaptureKeyringError)?;
    Ok(fingerprint)
}

#[cfg(test)]
fn write_new_current(root: &Path, anchors: &Path) -> Result<[u8; 32], CaptureKeyringError> {
    let key = create_versioned_key(root, anchors).map_err(|_| CaptureKeyringError)?;
    let fingerprint: [u8; 32] = Sha256::digest(key.as_ref()).into();
    let pending = root.join(format!(".current-{}", Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&pending)
        .map_err(|_| CaptureKeyringError)?;
    file.write_all(&fingerprint)
        .and_then(|_| file.sync_all())
        .map_err(|_| CaptureKeyringError)?;
    std::fs::rename(&pending, root.join("current"))
        .and_then(|_| std::fs::File::open(root)?.sync_all())
        .map_err(|_| CaptureKeyringError)?;
    Ok(fingerprint)
}

#[cfg(test)]
mod tests {
    use super::{current, initialize, locked_current, rotate};
    use crate::capture_master_key::load_versioned_key;
    use uuid::Uuid;

    #[test]
    fn rotation_keeps_previous_key_and_failed_rotation_keeps_current() {
        let root = std::env::temp_dir().join(format!("capture-rotation-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let keys = root.join("keys");
        let anchors = root.join("anchors");
        let spool = root.join("spool");
        std::fs::create_dir(&keys).unwrap();
        std::fs::create_dir(&anchors).unwrap();
        std::fs::create_dir(&spool).unwrap();
        assert!(current(&keys).is_err());
        let first = rotate(&keys, &anchors, &spool).unwrap();
        let old = current(&keys).unwrap();
        let second = rotate(&keys, &anchors, &spool).unwrap();
        assert_ne!(first, second);
        assert_ne!(*old, *current(&keys).unwrap());
        assert_eq!(*old, *load_versioned_key(&keys, &first).unwrap());
        assert!(rotate(&keys, &root.join("missing"), &spool).is_err());
        assert_eq!(
            *current(&keys).unwrap(),
            *load_versioned_key(&keys, &second).unwrap()
        );
        std::fs::write(keys.join("current"), [0u8; 31]).unwrap();
        assert!(current(&keys).is_err());
    }

    #[test]
    fn choosing_a_key_blocks_rotation_until_key_use_is_ready() {
        let root = std::env::temp_dir().join(format!("capture-key-lock-{}", Uuid::new_v4()));
        let keys = root.join("keys");
        let anchors = root.join("anchors");
        let spool = root.join("spool");
        std::fs::create_dir_all(&keys).unwrap();
        std::fs::create_dir_all(&anchors).unwrap();
        std::fs::create_dir_all(&spool).unwrap();
        rotate(&keys, &anchors, &spool).unwrap();
        let (old, guard) = locked_current(&keys).unwrap();
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(keys.join(".rotation.lock"))
            .unwrap();
        assert!(contender.try_lock().is_err());
        drop(guard);
        contender.try_lock().unwrap();
        drop(contender);
        rotate(&keys, &anchors, &spool).unwrap();
        assert_ne!(*old, *current(&keys).unwrap());
    }

    #[test]
    fn initialization_requires_empty_safe_directories_and_never_replaces_key() {
        use sha2::Digest;
        use std::os::unix::fs::PermissionsExt;
        let base =
            std::path::Path::new("/tmp").join(format!("capture-initialize-{}", Uuid::new_v4()));
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        let spool = base.join("spool");
        for path in [&base, &keys, &anchors, &spool] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(spool.join("existing.capture"), b"keep").unwrap();
        assert!(initialize(&keys, &anchors, &spool).is_err());
        assert!(!keys.join("current").exists());
        std::fs::remove_file(spool.join("existing.capture")).unwrap();
        let first = initialize(&keys, &anchors, &spool).unwrap();
        let old = current(&keys).unwrap();
        assert!(initialize(&keys, &anchors, &spool).is_err());
        assert_eq!(*old, *current(&keys).unwrap());
        assert_eq!(sha2::Sha256::digest(old.as_ref()).as_slice(), first);
    }

    #[test]
    fn concurrent_rotations_keep_every_created_key() {
        use std::sync::{Arc, Barrier};
        let root =
            std::env::temp_dir().join(format!("capture-rotate-concurrent-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let keys = root.join("keys");
        let anchors = root.join("anchors");
        let spool = root.join("spool");
        std::fs::create_dir(&keys).unwrap();
        std::fs::create_dir(&anchors).unwrap();
        std::fs::create_dir(&spool).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let keys = keys.clone();
                let anchors = anchors.clone();
                let spool = spool.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    rotate(&keys, &anchors, &spool).unwrap()
                })
            })
            .collect();
        barrier.wait();
        let versions: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_ne!(versions[0], versions[1]);
        let active = current(&keys).unwrap();
        let first = load_versioned_key(&keys, &versions[0]).unwrap();
        let second = load_versioned_key(&keys, &versions[1]).unwrap();
        assert!(*active == *first || *active == *second);
    }

    #[test]
    fn interrupted_initialization_reuses_the_persisted_key() {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;
        let base =
            std::path::Path::new("/tmp").join(format!("capture-key-resume-{}", Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        let spool = base.join("spool");
        for path in [&keys, &anchors, &spool] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let master = crate::capture_anchor::create_versioned_key(&keys, &anchors).unwrap();
        let mut intent = b"APGKINI1".to_vec();
        intent.extend_from_slice(master.as_ref());
        intent.extend_from_slice(&Sha256::digest(&intent));
        std::fs::write(keys.join(".initializing"), intent).unwrap();
        std::fs::set_permissions(
            keys.join(".initializing"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let expected: [u8; 32] = Sha256::digest(master.as_ref()).into();
        assert_eq!(initialize(&keys, &anchors, &spool).unwrap(), expected);
        assert_eq!(*current(&keys).unwrap(), *master);
        assert!(!keys.join(".initializing").exists());
        assert!(initialize(&keys, &anchors, &spool).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn interrupted_initialization_completes_matching_partial_files() {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;
        let key = [7u8; 32];
        let fingerprint: [u8; 32] = Sha256::digest(key).into();
        let name: String = fingerprint
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let anchor = crate::capture_anchor::initial_anchor(&fingerprint);
        let mut intent = b"APGKINI1".to_vec();
        intent.extend_from_slice(&key);
        let checksum = Sha256::digest(&intent);
        intent.extend_from_slice(&checksum);
        for phase in 0..4 {
            let base = std::path::Path::new("/tmp")
                .join(format!("capture-key-partial-{}", Uuid::new_v4()));
            std::fs::create_dir(&base).unwrap();
            std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
            let keys = base.join("keys");
            let anchors = base.join("anchors");
            let spool = base.join("spool");
            for path in [&keys, &anchors, &spool] {
                std::fs::create_dir(path).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let write = |path: std::path::PathBuf, bytes: &[u8]| {
                std::fs::write(&path, bytes).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
            };
            write(keys.join(".initializing"), &intent);
            if phase >= 1 {
                write(
                    keys.join(format!("{name}.key")),
                    &key[..if phase == 1 { 11 } else { 32 }],
                );
            }
            if phase >= 2 {
                write(
                    anchors.join(format!("{name}.anchor")),
                    &anchor[..if phase == 2 { 12 } else { anchor.len() }],
                );
            }
            if phase >= 3 {
                write(keys.join("current"), &fingerprint[..12]);
            }
            assert_eq!(initialize(&keys, &anchors, &spool).unwrap(), fingerprint);
            assert_eq!(*locked_current(&keys).unwrap().0, key);
            assert_eq!(
                std::fs::read(anchors.join(format!("{name}.anchor"))).unwrap(),
                anchor
            );
            std::fs::remove_dir_all(base).unwrap();
        }
    }

    #[test]
    fn interrupted_initialization_rejects_conflicts_without_overwriting() {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;
        let base =
            std::path::Path::new("/tmp").join(format!("capture-key-conflict-{}", Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        let spool = base.join("spool");
        for path in [&keys, &anchors, &spool] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let key = [7u8; 32];
        let fingerprint = Sha256::digest(key);
        let name: String = fingerprint
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let mut intent = b"APGKINI1".to_vec();
        intent.extend_from_slice(&key);
        let checksum = Sha256::digest(&intent);
        intent.extend_from_slice(&checksum);
        std::fs::write(keys.join(".initializing"), intent).unwrap();
        std::fs::set_permissions(
            keys.join(".initializing"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let bad = keys.join(format!("{name}.key"));
        std::fs::write(&bad, [9u8; 32]).unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(initialize(&keys, &anchors, &spool).is_err());
        assert_eq!(std::fs::read(&bad).unwrap(), [9u8; 32]);
        assert!(keys.join(".initializing").exists());
        assert!(locked_current(&keys).is_err());
        std::fs::write(&bad, key).unwrap();
        let mut used_anchor = crate::capture_anchor::initial_anchor(&fingerprint.into());
        used_anchor[40..48].copy_from_slice(&1_i64.to_be_bytes());
        let checksum = Sha256::digest(&used_anchor[..49]);
        used_anchor[49..].copy_from_slice(&checksum);
        let anchor_path = anchors.join(format!("{name}.anchor"));
        std::fs::write(&anchor_path, &used_anchor).unwrap();
        std::fs::set_permissions(&anchor_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(initialize(&keys, &anchors, &spool).is_err());
        assert_eq!(std::fs::read(anchor_path).unwrap(), used_anchor);
        assert!(keys.join(".initializing").exists());
        std::fs::remove_dir_all(base).unwrap();
    }
}
