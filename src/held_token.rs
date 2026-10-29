use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
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
    #[error("provider credential change could not be confirmed")]
    Uncertain,
}

#[derive(Clone)]
pub struct HeldToken {
    inner: Arc<Inner>,
}

pub struct PersonalTokens {
    root: PathBuf,
    api_key: bool,
    #[cfg(test)]
    fail_sync: Arc<std::sync::atomic::AtomicBool>,
}

impl PersonalTokens {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            api_key: false,
            #[cfg(test)]
            fail_sync: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub(crate) fn for_api(root: PathBuf, provider: &str) -> Self {
        let hash = Sha256::digest(provider.as_bytes());
        let encoded: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
        Self {
            root: root.join("api").join(encoded),
            api_key: true,
            #[cfg(test)]
            fail_sync: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub(crate) fn api_store(&self, provider: &str) -> Self {
        let store = Self::for_api(self.root.clone(), provider);
        #[cfg(test)]
        let store = Self {
            fail_sync: self.fail_sync.clone(),
            ..store
        };
        store
    }

    #[cfg(test)]
    pub(crate) fn fail_next_sync(&self) {
        self.fail_sync
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn path(&self, tenant: uuid::Uuid, user: uuid::Uuid) -> PathBuf {
        self.root.join(tenant.to_string()).join(user.to_string())
    }

    fn sync_directories(directory: &std::path::Path) -> Result<(), HeldTokenError> {
        for path in directory.ancestors() {
            let path = if path.as_os_str().is_empty() {
                std::path::Path::new(".")
            } else {
                path
            };
            std::fs::File::open(path)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| HeldTokenError::Uncertain)?;
        }
        Ok(())
    }

    fn clean_temporary(
        root: &std::path::Path,
        tenant: uuid::Uuid,
        user: uuid::Uuid,
    ) -> std::io::Result<()> {
        let path = root.join(tenant.to_string()).join(format!(".{user}.tmp"));
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn lock_user(
        root: &std::path::Path,
        tenant: uuid::Uuid,
        user: uuid::Uuid,
        shared: bool,
    ) -> std::io::Result<std::fs::File> {
        let directory = root.join(tenant.to_string());
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.join(format!(".{user}.lock")))?;
        if shared {
            file.try_lock_shared().map_err(std::io::Error::other)?;
        } else {
            file.try_lock().map_err(std::io::Error::other)?;
        }
        Ok(file)
    }

    #[cfg(test)]
    pub async fn get(
        &self,
        tenant: uuid::Uuid,
        user: uuid::Uuid,
    ) -> Result<Option<Arc<str>>, HeldTokenError> {
        Ok(self
            .get_with_lock(tenant, user)
            .await?
            .map(|(token, _)| token))
    }

    pub async fn get_with_lock(
        &self,
        tenant: uuid::Uuid,
        user: uuid::Uuid,
    ) -> Result<Option<(Arc<str>, std::fs::File)>, HeldTokenError> {
        let path = self.path(tenant, user);
        let root = self.root.clone();
        let api_key = self.api_key;
        tokio::task::spawn_blocking(move || {
            let lock = Self::lock_user(&root, tenant, user, true)?;
            match std::fs::read_to_string(path) {
                Ok(token) => {
                    normalize_personal(&token, api_key).map(|token| Some((Arc::from(token), lock)))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
            }
        })
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
    }

    pub async fn replace(
        &self,
        tenant: uuid::Uuid,
        user: uuid::Uuid,
        raw: &str,
    ) -> Result<String, HeldTokenError> {
        let token = normalize_personal(raw, self.api_key)?;
        let fingerprint = fingerprint(&token);
        let path = self.path(tenant, user);
        let root = self.root.clone();
        #[cfg(test)]
        let fail_sync = self.fail_sync.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = Self::lock_user(&root, tenant, user, false)?;
            Self::clean_temporary(&root, tenant, user)?;
            let parent = path
                .parent()
                .ok_or_else(|| std::io::Error::other("missing token directory"))?;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
            let temporary = parent.join(format!(".{user}.tmp"));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            let result = (|| -> Result<(), HeldTokenError> {
                file.write_all(token.as_bytes())?;
                file.sync_all()?;
                std::fs::rename(&temporary, &path)?;
                #[cfg(test)]
                if fail_sync.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    return Err(HeldTokenError::Uncertain);
                }
                Self::sync_directories(parent)
            })();
            if matches!(result, Err(HeldTokenError::Storage(_)))
                && let Err(error) = std::fs::remove_file(&temporary)
            {
                tracing::error!(error = %error, "temporary provider token cleanup failed");
            }
            result
        })
        .await
        .map_err(|_| HeldTokenError::Uncertain)??;
        Ok(fingerprint)
    }

    pub async fn revoke(&self, tenant: uuid::Uuid, user: uuid::Uuid) -> Result<(), HeldTokenError> {
        let path = self.path(tenant, user);
        let root = self.root.clone();
        #[cfg(test)]
        let fail_sync = self.fail_sync.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = Self::lock_user(&root, tenant, user, false)?;
            Self::clean_temporary(&root, tenant, user)?;
            let parent = path
                .parent()
                .ok_or_else(|| std::io::Error::other("missing token directory"))?;
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    #[cfg(test)]
                    if fail_sync.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        return Err(HeldTokenError::Uncertain);
                    }
                    Self::sync_directories(parent)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            }
        })
        .await
        .map_err(|_| HeldTokenError::Uncertain)??;
        Ok(())
    }
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

fn normalize_personal(raw: &str, api_key: bool) -> Result<String, HeldTokenError> {
    if !api_key {
        return normalize(raw);
    }
    let value = raw.trim();
    if !value.is_empty() && value.len() <= MAX_LEN && value.chars().all(|c| c.is_ascii_graphic()) {
        Ok(value.to_owned())
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

pub fn validated_fingerprint(token: &str) -> Result<String, HeldTokenError> {
    Ok(fingerprint(&normalize(token)?))
}

pub fn validated_api_fingerprint(token: &str) -> Result<String, HeldTokenError> {
    Ok(fingerprint(&normalize_personal(token, true)?))
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

    #[tokio::test]
    async fn personal_tokens_do_not_cross_users_or_tenants() {
        let root = scratch("personal");
        std::fs::create_dir_all(&root).unwrap();
        let store = PersonalTokens::new(root);
        let tenant = uuid::Uuid::new_v4();
        let other_tenant = uuid::Uuid::new_v4();
        let alice = uuid::Uuid::new_v4();
        let bob = uuid::Uuid::new_v4();
        store
            .replace(tenant, alice, "sk-ant-oat01-alice")
            .await
            .unwrap();
        store
            .replace(tenant, bob, "sk-ant-oat01-bob")
            .await
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(store.path(tenant, alice))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(store.path(tenant, alice).parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            &*store.get(tenant, alice).await.unwrap().unwrap(),
            "sk-ant-oat01-alice"
        );
        assert_eq!(
            &*store.get(tenant, bob).await.unwrap().unwrap(),
            "sk-ant-oat01-bob"
        );
        assert!(store.replace(tenant, alice, "not-a-token").await.is_err());
        assert_eq!(
            &*store.get(tenant, alice).await.unwrap().unwrap(),
            "sk-ant-oat01-alice"
        );
        assert!(store.get(other_tenant, alice).await.unwrap().is_none());
        assert!(
            store
                .get(tenant, uuid::Uuid::new_v4())
                .await
                .unwrap()
                .is_none()
        );
        store.revoke(tenant, alice).await.unwrap();
        assert!(store.get(tenant, alice).await.unwrap().is_none());
        assert_eq!(
            &*store.get(tenant, bob).await.unwrap().unwrap(),
            "sk-ant-oat01-bob"
        );
    }

    #[tokio::test]
    #[ignore = "run separately because process creation can temporarily inherit other tests' locks"]
    async fn personal_tokens_survive_process_restart_and_revocation() {
        if let Ok(root) = std::env::var("GATEWAY_TEST_RESTART_ROOT") {
            let tenant = std::env::var("GATEWAY_TEST_RESTART_TENANT")
                .unwrap()
                .parse()
                .unwrap();
            let user = std::env::var("GATEWAY_TEST_RESTART_USER")
                .unwrap()
                .parse()
                .unwrap();
            let other = std::env::var("GATEWAY_TEST_RESTART_OTHER")
                .unwrap()
                .parse()
                .unwrap();
            let store = PersonalTokens::new(root.into());
            match std::env::var("GATEWAY_TEST_RESTART_ACTION").as_deref() {
                Ok("write") => {
                    store
                        .replace(tenant, user, "sk-ant-oat01-restart-test")
                        .await
                        .unwrap();
                    return;
                }
                Ok("verify_revoked") => {
                    assert!(store.get(tenant, user).await.unwrap().is_none());
                    return;
                }
                _ => {}
            }
            assert_eq!(
                &*store.get(tenant, user).await.unwrap().unwrap(),
                "sk-ant-oat01-restart-test"
            );
            assert!(store.get(tenant, other).await.unwrap().is_none());
            store.revoke(tenant, user).await.unwrap();
            return;
        }
        let root = std::env::temp_dir().join(format!("gateway-restart-{}", uuid::Uuid::new_v4()));
        let tenant = uuid::Uuid::new_v4();
        let user = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let store = PersonalTokens::new(root.clone());
        store
            .replace(tenant, user, "sk-ant-oat01-restart-test")
            .await
            .unwrap();
        drop(store);
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "held_token::tests::personal_tokens_survive_process_restart_and_revocation",
                "--ignored",
                "--nocapture",
            ])
            .env("GATEWAY_TEST_RESTART_ROOT", &root)
            .env("GATEWAY_TEST_RESTART_TENANT", tenant.to_string())
            .env("GATEWAY_TEST_RESTART_USER", user.to_string())
            .env("GATEWAY_TEST_RESTART_OTHER", other.to_string())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
        let restarted = PersonalTokens::new(root);
        assert!(restarted.get(tenant, user).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn personal_tokens_reject_when_shared_lock_is_held() {
        let root = scratch("shared-lock");
        std::fs::create_dir_all(&root).unwrap();
        let tenant = uuid::Uuid::new_v4();
        let user = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let lock = PersonalTokens::lock_user(&root, tenant, user, false).unwrap();
        let store = Arc::new(PersonalTokens::new(root));
        assert!(
            store
                .replace(tenant, user, "sk-ant-oat01-new")
                .await
                .is_err()
        );
        store
            .replace(tenant, other, "sk-ant-oat01-other")
            .await
            .unwrap();
        drop(lock);
        store
            .replace(tenant, user, "sk-ant-oat01-new")
            .await
            .unwrap();
        assert_eq!(
            &*store.get(tenant, user).await.unwrap().unwrap(),
            "sk-ant-oat01-new"
        );
    }

    #[tokio::test]
    async fn personal_token_parallel_reads_do_not_block_each_other() {
        let root = scratch("shared-read");
        let tenant = uuid::Uuid::new_v4();
        let user = uuid::Uuid::new_v4();
        let store = PersonalTokens::new(root.clone());
        store
            .replace(tenant, user, "sk-ant-oat01-user")
            .await
            .unwrap();
        let read_lock = PersonalTokens::lock_user(&root, tenant, user, true).unwrap();
        assert_eq!(
            &*store.get(tenant, user).await.unwrap().unwrap(),
            "sk-ant-oat01-user"
        );
        assert!(store.revoke(tenant, user).await.is_err());
        drop(read_lock);
        store.revoke(tenant, user).await.unwrap();
        assert!(store.get(tenant, user).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn failed_personal_token_replace_removes_temporary_secret() {
        let root = scratch("failed-replace");
        let tenant = uuid::Uuid::new_v4();
        let user = uuid::Uuid::new_v4();
        let store = PersonalTokens::new(root);
        let destination = store.path(tenant, user);
        std::fs::create_dir_all(&destination).unwrap();
        assert!(
            store
                .replace(tenant, user, "sk-ant-oat01-test")
                .await
                .is_err()
        );
        let entries = std::fs::read_dir(destination.parent().unwrap()).unwrap();
        let names: Vec<_> = entries.map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(names.len(), 2);
        assert!(destination.is_dir());
    }

    #[tokio::test]
    async fn interrupted_temporary_token_is_removed_only_for_its_owner() {
        let root = scratch("interrupted-token");
        let tenant = uuid::Uuid::new_v4();
        let user = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let parent = root.join(tenant.to_string());
        std::fs::create_dir_all(&parent).unwrap();
        let stale = parent.join(format!(".{user}.tmp"));
        let other_stale = parent.join(format!(".{other}.tmp"));
        std::fs::write(&stale, "sk-ant-oat01-interrupted").unwrap();
        std::fs::write(&other_stale, "sk-ant-oat01-other").unwrap();
        let store = PersonalTokens::new(root);
        store.revoke(tenant, user).await.unwrap();
        assert!(!stale.exists());
        assert!(other_stale.exists());
        store
            .replace(tenant, user, "sk-ant-oat01-new")
            .await
            .unwrap();
        assert_eq!(
            &*store.get(tenant, user).await.unwrap().unwrap(),
            "sk-ant-oat01-new"
        );
    }

    #[tokio::test]
    async fn personal_api_keys_are_isolated_by_user_tenant_and_provider() {
        let root = scratch("personal-api");
        let tenant = uuid::Uuid::new_v4();
        let alice = uuid::Uuid::new_v4();
        let bob = uuid::Uuid::new_v4();
        let openai = PersonalTokens::for_api(root.clone(), "openai-primary");
        let other_openai = PersonalTokens::for_api(root.clone(), "openai-secondary");
        let gemini = PersonalTokens::for_api(root, "gemini");
        openai
            .replace(tenant, alice, "alice-openai-key")
            .await
            .unwrap();
        openai.replace(tenant, bob, "bob-openai-key").await.unwrap();
        gemini
            .replace(tenant, alice, "alice-gemini-key")
            .await
            .unwrap();
        assert_eq!(
            &*openai.get(tenant, alice).await.unwrap().unwrap(),
            "alice-openai-key"
        );
        assert_eq!(
            &*openai.get(tenant, bob).await.unwrap().unwrap(),
            "bob-openai-key"
        );
        assert_eq!(
            &*gemini.get(tenant, alice).await.unwrap().unwrap(),
            "alice-gemini-key"
        );
        assert!(gemini.get(tenant, bob).await.unwrap().is_none());
        assert!(other_openai.get(tenant, alice).await.unwrap().is_none());
        assert!(
            openai
                .get(uuid::Uuid::new_v4(), alice)
                .await
                .unwrap()
                .is_none()
        );
        assert!(openai.replace(tenant, alice, "has spaces").await.is_err());
        openai.revoke(tenant, alice).await.unwrap();
        assert!(openai.get(tenant, alice).await.unwrap().is_none());
        assert_eq!(
            &*gemini.get(tenant, alice).await.unwrap().unwrap(),
            "alice-gemini-key"
        );
    }

    #[tokio::test]
    async fn personal_tokens_respect_lock_held_by_another_process() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};

        let root = scratch("process-lock");
        let tenant = uuid::Uuid::new_v4();
        let user = uuid::Uuid::new_v4();
        let directory = root.join(tenant.to_string());
        std::fs::create_dir_all(&directory).unwrap();
        let mut child = Command::new("flock")
            .arg(directory.join(format!(".{user}.lock")))
            .args(["sh", "-c", "printf 'ready\\n'; read release"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, "ready\n");
        let store = PersonalTokens::new(root);
        assert!(
            store
                .replace(tenant, user, "sk-ant-oat01-new")
                .await
                .is_err()
        );
        child.stdin.take().unwrap().write_all(b"release\n").unwrap();
        assert!(child.wait().unwrap().success());
        store
            .replace(tenant, user, "sk-ant-oat01-new")
            .await
            .unwrap();
    }
}
