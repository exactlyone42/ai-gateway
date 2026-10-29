use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
#[error("content storage directories are unavailable or unsafe")]
pub(crate) struct CapturePathsError;

pub(crate) fn validate_paths(paths: [&Path; 3]) -> Result<(), CapturePathsError> {
    let mut resolved = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = std::fs::symlink_metadata(path).map_err(|_| CapturePathsError)?;
        if !metadata.is_dir() || metadata.mode() & 0o022 != 0 {
            return Err(CapturePathsError);
        }
        let canonical = path.canonicalize().map_err(|_| CapturePathsError)?;
        if !path.is_absolute() || path != canonical {
            return Err(CapturePathsError);
        }
        let mut child = canonical.as_path();
        while let Some(parent) = child.parent() {
            let parent_meta = std::fs::metadata(parent).map_err(|_| CapturePathsError)?;
            if parent_meta.mode() & 0o022 != 0 {
                let owner = std::fs::metadata(child)
                    .map_err(|_| CapturePathsError)?
                    .uid();
                let current = unsafe { libc::geteuid() };
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
