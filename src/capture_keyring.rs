use crate::capture_anchor::create_versioned_key;
use crate::capture_master_key::load_versioned_key;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::io::Write;
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
    Ok((current(root)?, lock))
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
    for (path, allowed) in [
        (root, Some(".rotation.lock")),
        (anchors, None),
        (spool, Some(".capture-backup.lock")),
    ] {
        for entry in std::fs::read_dir(path).map_err(|_| CaptureKeyringError)? {
            let entry = entry.map_err(|_| CaptureKeyringError)?;
            if allowed.is_none_or(|name| entry.file_name() != name) {
                return Err(CaptureKeyringError);
            }
        }
    }
    write_new_current(root, anchors)
}

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
}
