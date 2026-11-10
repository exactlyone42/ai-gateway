use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
#[error("capture master key unavailable")]
pub(crate) struct CaptureMasterKeyError;

pub(crate) fn load_versioned_key(
    root: &Path,
    fingerprint: &[u8; 32],
) -> Result<Zeroizing<[u8; 32]>, CaptureMasterKeyError> {
    let name: String = fingerprint
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let key = load_master_key(&root.join(format!("{name}.key")))?;
    if Sha256::digest(key.as_ref()).as_slice() != fingerprint {
        return Err(CaptureMasterKeyError);
    }
    Ok(key)
}

pub(crate) fn load_master_key(path: &Path) -> Result<Zeroizing<[u8; 32]>, CaptureMasterKeyError> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| CaptureMasterKeyError)?;
    let metadata = file.metadata().map_err(|_| CaptureMasterKeyError)?;
    if !metadata.is_file() || metadata.mode() & 0o077 != 0 || metadata.len() != 32 {
        return Err(CaptureMasterKeyError);
    }
    let mut key = Zeroizing::new([0u8; 32]);
    file.read_exact(key.as_mut())
        .map_err(|_| CaptureMasterKeyError)?;
    let mut extra = [0u8; 1];
    if file.read(&mut extra).map_err(|_| CaptureMasterKeyError)? != 0 {
        return Err(CaptureMasterKeyError);
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::load_master_key;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use uuid::Uuid;

    #[test]
    fn reads_only_private_regular_exact_length_key() {
        let root = std::env::temp_dir().join(format!("capture-key-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("key");
        std::fs::write(&path, [7u8; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(*load_master_key(&path).unwrap(), [7u8; 32]);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(load_master_key(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, [7u8; 31]).unwrap();
        assert!(load_master_key(&path).is_err());
        std::fs::write(&path, [7u8; 33]).unwrap();
        assert!(load_master_key(&path).is_err());
        std::fs::write(&path, [7u8; 32]).unwrap();
        let link = root.join("link");
        symlink(&path, &link).unwrap();
        assert!(load_master_key(&link).is_err());
        assert!(load_master_key(&root).is_err());
        assert!(load_master_key(&root.join("missing")).is_err());
    }
}
