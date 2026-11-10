use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
#[error("content storage directories are unavailable or unsafe")]
pub(crate) struct CapturePathsError;

pub(crate) fn validate_paths<const N: usize>(paths: [&Path; N]) -> Result<(), CapturePathsError> {
    let mut resolved = Vec::with_capacity(paths.len());
    let current = unsafe { libc::geteuid() };
    for path in paths {
        let metadata = std::fs::symlink_metadata(path).map_err(|_| CapturePathsError)?;
        if !metadata.is_dir()
            || metadata.mode() & 0o022 != 0
            || (metadata.uid() != 0 && metadata.uid() != current)
        {
            return Err(CapturePathsError);
        }
        let canonical = path.canonicalize().map_err(|_| CapturePathsError)?;
        if !path.is_absolute() || path != canonical {
            return Err(CapturePathsError);
        }
        let mut child = canonical.as_path();
        while let Some(parent) = child.parent() {
            let parent_meta = std::fs::metadata(parent).map_err(|_| CapturePathsError)?;
            if parent_meta.uid() != 0 && parent_meta.uid() != current {
                return Err(CapturePathsError);
            }
            if parent_meta.mode() & 0o022 != 0 {
                let owner = std::fs::metadata(child)
                    .map_err(|_| CapturePathsError)?
                    .uid();
                if parent_meta.mode() & 0o1000 == 0 || (owner != current && owner != 0) {
                    return Err(CapturePathsError);
                }
            }
            child = parent;
        }
        if resolved
            .iter()
            .any(|other: &PathBuf| canonical.starts_with(other) || other.starts_with(&canonical))
        {
            return Err(CapturePathsError);
        }
        resolved.push(canonical);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_paths;
    use std::os::unix::fs::PermissionsExt;
    use uuid::Uuid;

    #[test]
    #[ignore = "requires independently owned test directory"]
    fn rejects_a_directory_with_an_untrusted_owner() {
        let foreign = std::env::var_os("GATEWAY_TEST_UNTRUSTED_WITNESS").unwrap();
        let root =
            std::path::Path::new("/tmp").join(format!("capture-paths-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(validate_paths([root.as_path()]).is_ok());
        assert!(validate_paths([std::path::Path::new(&foreign)]).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
