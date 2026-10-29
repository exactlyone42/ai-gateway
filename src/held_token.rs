use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};

const PREFIX: &str = "sk-ant-oat";
const MAX_LEN: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum HeldTokenError {
    #[error("subscription token has an invalid format")]
    Invalid,
    #[error("subscription token could not be stored")]
    Storage(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct HeldToken {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    current: RwLock<Option<Arc<str>>>,
}

fn normalize(raw: &str) -> Result<String, HeldTokenError> {
    let token = raw.trim();
    if token.starts_with(PREFIX)
        && token.len() <= MAX_LEN
        && token.chars().all(|c| c.is_ascii_graphic())
    {
        Ok(token.to_owned())
    } else {
        Err(HeldTokenError::Invalid)
    }
}

pub fn fingerprint(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl HeldToken {
    pub fn load(path: PathBuf, seed: Option<&str>) -> Result<Self, HeldTokenError> {
        let stored = match std::fs::read_to_string(&path) {
            Ok(text) => Some(normalize(&text)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let token = match (stored, seed) {
            (Some(token), _) => Some(token),
            (None, Some(seed)) => {
                let token = normalize(seed)?;
                write_file(&path, &token)?;
                Some(token)
            }
            (None, None) => None,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                path,
                current: RwLock::new(token.map(Arc::from)),
            }),
        })
    }

    pub fn get(&self) -> Option<Arc<str>> {
        self.inner
            .current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub async fn replace(&self, raw: &str) -> Result<String, HeldTokenError> {
        let token = normalize(raw)?;
        let path = self.inner.path.clone();
        let to_write = token.clone();
        tokio::task::spawn_blocking(move || write_file(&path, &to_write))
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))??;
        *self
            .inner
            .current
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::from(token.as_str()));
        Ok(fingerprint(&token))
    }
}

fn write_file(path: &std::path::Path, token: &str) -> std::io::Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("held-token-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("token")
    }

    #[tokio::test]
    async fn replacement_is_stored_with_private_mode_and_survives_reload() {
        let path = scratch("reload");
        let held = HeldToken::load(path.clone(), None).unwrap();
        assert!(held.get().is_none());
        let first = held.replace("sk-ant-oat01-first").await.unwrap();
        let second = held.replace(" sk-ant-oat01-second\n").await.unwrap();
        assert_ne!(first, second);
        assert_eq!(&*held.get().unwrap(), "sk-ant-oat01-second");
        let reloaded = HeldToken::load(path.clone(), Some("sk-ant-oat01-ignored")).unwrap();
        assert_eq!(&*reloaded.get().unwrap(), "sk-ant-oat01-second");
        let mode = std::fs::metadata(&path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.permissions().mode() & 0o777, 0o600);
    }

    #[tokio::test]
    async fn invalid_tokens_are_rejected_and_keep_the_old_one() {
        let held = HeldToken::load(scratch("invalid"), Some("sk-ant-oat01-seed")).unwrap();
        for bad in ["", "sk-ant-api03-x", "sk-ant-oat01 two", "sk-ant-oat01-é"] {
            assert!(matches!(
                held.replace(bad).await,
                Err(HeldTokenError::Invalid)
            ));
        }
        assert_eq!(&*held.get().unwrap(), "sk-ant-oat01-seed");
    }

    #[test]
    fn seed_is_used_only_when_no_stored_token_exists() {
        let path = scratch("seed");
        let held = HeldToken::load(path, Some("sk-ant-oat01-seed")).unwrap();
        assert_eq!(&*held.get().unwrap(), "sk-ant-oat01-seed");
    }
}
