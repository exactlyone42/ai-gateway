use crate::capture_anchor::reserve_anchored;
use crate::capture_backup_barrier::{CaptureBarrierLease, try_shared};
use crate::capture_crypto::{CaptureIdentity, CaptureLeg};
use crate::capture_paths::validate_paths;
use crate::capture_spool::{CaptureLimits, CaptureSpool, CaptureSpoolError};
use sqlx::PgPool;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

const MAX_BLOCK_BYTES: usize = 1024 * 1024;

pub(crate) struct CaptureSession {
    spool: CaptureSpool,
    next: [u64; 4],
    ready: bool,
    failed: bool,
    _backup_guard: CaptureBarrierLease,
}

pub(crate) struct AsyncCaptureSession {
    session: Arc<Mutex<Option<CaptureSession>>>,
    capacity: Arc<Semaphore>,
    cancelled: Arc<AtomicBool>,
}

pub(crate) struct CapturePaths<'a> {
    pub spool: &'a Path,
    pub anchors: &'a Path,
}

struct CancellationGuard {
    cancelled: Arc<AtomicBool>,
    complete: bool,
}

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

impl AsyncCaptureSession {
    fn new(session: CaptureSession, capacity: Arc<Semaphore>) -> Self {
        Self {
            session: Arc::new(Mutex::new(Some(session))),
            capacity,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    pub(crate) async fn create(
        pool: &PgPool,
        paths: CapturePaths<'_>,
        identity: CaptureIdentity,
        master: &[u8; 32],
        secrets: &[&[u8]],
        limits: CaptureLimits,
        capacity: Arc<Semaphore>,
    ) -> Result<Self, CaptureSpoolError> {
        Self::create_inner(
            pool,
            paths,
            identity,
            (master, None),
            secrets,
            limits,
            capacity,
        )
        .await
    }

    async fn create_inner(
        pool: &PgPool,
        paths: CapturePaths<'_>,
        identity: CaptureIdentity,
        key: (&[u8; 32], Option<std::fs::File>),
        secrets: &[&[u8]],
        limits: CaptureLimits,
        capacity: Arc<Semaphore>,
    ) -> Result<Self, CaptureSpoolError> {
        let (master, key_guard) = key;
        if secrets.len() > 32
            || secrets
                .iter()
                .any(|secret| secret.is_empty() || secret.len() > 4096)
        {
            return Err(CaptureSpoolError::Unavailable);
        }
        let permit = capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let guard_root = paths.spool.to_owned();
        let (backup_guard, permit) = tokio::task::spawn_blocking(move || {
            let guard = try_shared(&guard_root).map_err(|_| CaptureSpoolError::Unavailable)?;
            Ok::<_, CaptureSpoolError>((guard, permit))
        })
        .await
        .map_err(|_| CaptureSpoolError::Unavailable)??;
        let wrap = reserve_anchored(pool, paths.anchors, master)
            .await
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let root = paths.spool.to_owned();
        let master = Zeroizing::new(*master);
        let secrets: Vec<Zeroizing<Vec<u8>>> = secrets
            .iter()
            .map(|secret| Zeroizing::new(secret.to_vec()))
            .collect();
        let session = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _key_guard = key_guard;
            let borrowed: Vec<&[u8]> = secrets.iter().map(|secret| secret.as_slice()).collect();
            let spool = CaptureSpool::create_with_permit(
                &root, identity, &master, &borrowed, wrap, limits,
            )?;
            Ok::<_, CaptureSpoolError>(CaptureSession {
                spool,
                next: [0; 4],
                ready: false,
                failed: false,
                _backup_guard: backup_guard,
            })
        })
        .await
        .map_err(|_| CaptureSpoolError::Unavailable)??;
        Ok(Self::new(session, capacity))
    }

    pub(crate) async fn create_from_keyring(
        pool: &PgPool,
        paths: CapturePaths<'_>,
        key_root: &Path,
        identity: CaptureIdentity,
        secrets: &[&[u8]],
        limits: CaptureLimits,
        capacity: Arc<Semaphore>,
    ) -> Result<Self, CaptureSpoolError> {
        let permit = capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let key_root = key_root.to_owned();
        let spool = paths.spool.to_owned();
        let anchors = paths.anchors.to_owned();
        let (master, key_guard) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            validate_paths([&spool, &key_root, &anchors])
                .map_err(|_| CaptureSpoolError::Unavailable)?;
            crate::capture_keyring::locked_current(&key_root)
                .map_err(|_| CaptureSpoolError::Unavailable)
        })
        .await
        .map_err(|_| CaptureSpoolError::Unavailable)??;
        Self::create_inner(
            pool,
            paths,
            identity,
            (&master, Some(key_guard)),
            secrets,
            limits,
            capacity,
        )
        .await
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Option<CaptureSession>) -> Result<T, CaptureSpoolError>
        + Send
        + 'static,
    ) -> Result<T, CaptureSpoolError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(CaptureSpoolError::Unavailable);
        }
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| CaptureSpoolError::Unavailable)?;
        let session = self.session.clone();
        let cancelled = self.cancelled.clone();
        let mut guard = CancellationGuard {
            cancelled: cancelled.clone(),
            complete: false,
        };
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut session = session.lock().map_err(|_| CaptureSpoolError::Unavailable)?;
            if cancelled.load(Ordering::Acquire) {
                return Err(CaptureSpoolError::Unavailable);
            }
            operation(&mut session)
        })
        .await
        .map_err(|_| CaptureSpoolError::Unavailable)?;
        guard.complete = true;
        result
    }

    pub(crate) async fn save_requests(
        &self,
        client: Vec<u8>,
        provider: Vec<u8>,
    ) -> Result<(), CaptureSpoolError> {
        self.run(move |session| {
            session
                .as_mut()
                .ok_or(CaptureSpoolError::Unavailable)?
                .save_requests(&client, &provider)
        })
        .await
    }

    pub(crate) async fn save_response(
        &self,
        leg: CaptureLeg,
        bytes: Vec<u8>,
    ) -> Result<(), CaptureSpoolError> {
        self.run(move |session| {
            session
                .as_mut()
                .ok_or(CaptureSpoolError::Unavailable)?
                .save_response(leg, &bytes)
        })
        .await
    }

    pub(crate) async fn finish(&self) -> Result<(), CaptureSpoolError> {
        self.run(|session| {
            session
                .take()
                .ok_or(CaptureSpoolError::Unavailable)?
                .finish()
        })
        .await
    }
}

impl CaptureSession {
    #[cfg(test)]
    pub(crate) async fn create(
        pool: &PgPool,
        root: &Path,
        identity: CaptureIdentity,
        master: &[u8; 32],
        secrets: &[&[u8]],
        limits: CaptureLimits,
    ) -> Result<Self, CaptureSpoolError> {
        let backup_guard = try_shared(root).map_err(|_| CaptureSpoolError::Unavailable)?;
        let spool = CaptureSpool::create(pool, root, identity, master, secrets, limits).await?;
        Ok(Self {
            spool,
            next: [0; 4],
            ready: false,
            failed: false,
            _backup_guard: backup_guard,
        })
    }

    pub(crate) fn save_requests(
        &mut self,
        client: &[u8],
        provider: &[u8],
    ) -> Result<(), CaptureSpoolError> {
        if self.ready || self.failed {
            return Err(CaptureSpoolError::Unavailable);
        }
        self.save(CaptureLeg::ClientRequest, client)?;
        self.save(CaptureLeg::ProviderRequest, provider)?;
        self.ready = true;
        Ok(())
    }

    pub(crate) fn may_send_upstream(&self) -> bool {
        self.ready && !self.failed
    }

    pub(crate) fn save_response(
        &mut self,
        leg: CaptureLeg,
        bytes: &[u8],
    ) -> Result<(), CaptureSpoolError> {
        if !self.may_send_upstream()
            || !matches!(
                leg,
                CaptureLeg::ProviderResponse | CaptureLeg::ClientResponse
            )
        {
            return Err(CaptureSpoolError::Unavailable);
        }
        self.save(leg, bytes)
    }

    fn save(&mut self, leg: CaptureLeg, bytes: &[u8]) -> Result<(), CaptureSpoolError> {
        let index = leg.index();
        let mut chunks = bytes.chunks(MAX_BLOCK_BYTES).peekable();
        if chunks.peek().is_none() {
            if self.spool.append(leg, self.next[index], &[]).is_err() {
                self.failed = true;
                return Err(CaptureSpoolError::Unavailable);
            }
            self.next[index] += 1;
            return Ok(());
        }
        for chunk in chunks {
            if self.spool.append(leg, self.next[index], chunk).is_err() {
                self.failed = true;
                return Err(CaptureSpoolError::Unavailable);
            }
            self.next[index] += 1;
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<(), CaptureSpoolError> {
        if !self.may_send_upstream() || self.next[2] == 0 || self.next[3] == 0 {
            return Err(CaptureSpoolError::Unavailable);
        }
        self.spool.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{AsyncCaptureSession, CapturePaths, CaptureSession};
    use crate::capture_anchor::create_key;
    use crate::capture_crypto::{CaptureIdentity, CaptureLeg};
    use crate::capture_spool::CaptureSpool;
    use sqlx::PgPool;
    use std::os::unix::fs::PermissionsExt;
    use uuid::Uuid;

    fn protect(paths: [&std::path::Path; 3]) {
        for path in paths {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    #[test]
    fn capture_directories_must_be_distinct_and_protected() {
        use std::os::unix::fs::symlink;
        let base = std::path::Path::new("/tmp").join(format!("capture-paths-{}", Uuid::new_v4()));
        let spool = base.join("spool");
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        std::fs::create_dir_all(&spool).unwrap();
        std::fs::create_dir_all(&keys).unwrap();
        std::fs::create_dir_all(&anchors).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        protect([&spool, &keys, &anchors]);
        assert!(super::validate_paths([&spool, &keys, &anchors]).is_ok());
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(super::validate_paths([&spool, &keys, &anchors]).is_err());
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(super::validate_paths([&spool, &spool, &anchors]).is_err());
        let nested = spool.join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(super::validate_paths([&spool, &nested, &anchors]).is_err());
        let alias = base.join("alias");
        symlink(&keys, &alias).unwrap();
        assert!(super::validate_paths([&spool, &alias, &anchors]).is_err());
        let parent_alias = base.join("parent-alias");
        symlink(&base, &parent_alias).unwrap();
        assert!(super::validate_paths([&spool, &parent_alias.join("keys"), &anchors]).is_err());
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(super::validate_paths([&spool, &keys, &anchors]).is_err());
    }

    #[sqlx::test]
    async fn new_sessions_use_current_key_and_old_sessions_remain_readable(pool: PgPool) {
        use crate::capture_keyring::rotate;
        use std::sync::Arc;
        use tokio::sync::Semaphore;
        let base = std::path::Path::new("/tmp").join(format!("capture-current-{}", Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = base.join("spool");
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&keys).unwrap();
        std::fs::create_dir(&anchors).unwrap();
        protect([&root, &keys, &anchors]);
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let capacity = Arc::new(Semaphore::new(1));
        let first_fingerprint = rotate(&keys, &anchors, &root).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            AsyncCaptureSession::create_from_keyring(
                &pool,
                CapturePaths {
                    spool: &root,
                    anchors: &anchors
                },
                &keys,
                identity,
                &[],
                crate::capture_spool::CaptureLimits::for_test(4096),
                capacity.clone(),
            )
            .await
            .is_err()
        );
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            AsyncCaptureSession::create_from_keyring(
                &pool,
                CapturePaths {
                    spool: &root,
                    anchors: &anchors
                },
                &keys,
                identity,
                &[],
                crate::capture_spool::CaptureLimits::for_test(4096),
                capacity.clone(),
            )
            .await
            .is_err()
        );
        let reserved: i64 = sqlx::query_scalar("SELECT count(*) FROM capture_master_key_usage")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(reserved, 0);
        assert!(!root.join(format!("{}.capture", identity.capture)).exists());
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o700)).unwrap();
        let first = AsyncCaptureSession::create_from_keyring(
            &pool,
            CapturePaths {
                spool: &root,
                anchors: &anchors,
            },
            &keys,
            identity,
            &[],
            crate::capture_spool::CaptureLimits::for_test(4096),
            capacity.clone(),
        )
        .await
        .unwrap();
        first
            .save_requests(b"first".to_vec(), b"first".to_vec())
            .await
            .unwrap();
        first
            .save_response(CaptureLeg::ProviderResponse, b"reply".to_vec())
            .await
            .unwrap();
        first
            .save_response(CaptureLeg::ClientResponse, b"reply".to_vec())
            .await
            .unwrap();
        first.finish().await.unwrap();
        let second_fingerprint = rotate(&keys, &anchors, &root).unwrap();
        let second_identity = CaptureIdentity {
            capture: Uuid::new_v4(),
            ..identity
        };
        let second = AsyncCaptureSession::create_from_keyring(
            &pool,
            CapturePaths {
                spool: &root,
                anchors: &anchors,
            },
            &keys,
            second_identity,
            &[],
            crate::capture_spool::CaptureLimits::for_test(4096),
            capacity,
        )
        .await
        .unwrap();
        second
            .save_requests(b"second".to_vec(), b"second".to_vec())
            .await
            .unwrap();
        second
            .save_response(CaptureLeg::ProviderResponse, b"reply".to_vec())
            .await
            .unwrap();
        second
            .save_response(CaptureLeg::ClientResponse, b"reply".to_vec())
            .await
            .unwrap();
        second.finish().await.unwrap();
        let old = CaptureSpool::recover_with_keyring(
            &root.join(format!("{}.capture", identity.capture)),
            &keys,
            4096,
        )
        .unwrap();
        let new = CaptureSpool::recover_with_keyring(
            &root.join(format!("{}.capture", second_identity.capture)),
            &keys,
            4096,
        )
        .unwrap();
        assert_eq!(*old.cipher.fingerprint(), first_fingerprint);
        assert_eq!(*new.cipher.fingerprint(), second_fingerprint);
        assert_ne!(first_fingerprint, second_fingerprint);
    }

    #[sqlx::test]
    async fn cancellation_during_spool_creation_does_not_release_key_rotation_lock(pool: PgPool) {
        use std::os::unix::fs::MetadataExt;
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::Semaphore;

        let base =
            std::path::Path::new("/tmp").join(format!("capture-create-cancel-{}", Uuid::new_v4()));
        let spool = base.join("spool");
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        std::fs::create_dir_all(&spool).unwrap();
        std::fs::create_dir_all(&keys).unwrap();
        std::fs::create_dir_all(&anchors).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        protect([&spool, &keys, &anchors]);
        let old = crate::capture_keyring::rotate(&keys, &anchors, &spool).unwrap();
        let lock_path = spool.join(".capture-capacity.lock");
        std::fs::write(&lock_path, (4096u64 * 16).to_be_bytes()).unwrap();
        let disk_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        disk_lock.lock().unwrap();
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(keys.join(".rotation.lock"))
            .unwrap();
        let capacity = Arc::new(Semaphore::new(1));
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let worker_pool = pool.clone();
        let worker_spool = spool.clone();
        let worker_anchors = anchors.clone();
        let worker_keys = keys.clone();
        let worker_capacity = capacity.clone();
        let worker = tokio::spawn(async move {
            AsyncCaptureSession::create_from_keyring(
                &worker_pool,
                CapturePaths {
                    spool: &worker_spool,
                    anchors: &worker_anchors,
                },
                &worker_keys,
                identity,
                &[],
                crate::capture_spool::CaptureLimits::for_test(4096),
                worker_capacity,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let reserved: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM capture_master_key_usage")
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                let inode = std::fs::metadata(&lock_path).unwrap().ino();
                let opened = std::fs::read_dir("/proc/self/fd")
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        std::fs::metadata(entry.path()).is_ok_and(|meta| meta.ino() == inode)
                    })
                    .count();
                if reserved == 1 && capacity.available_permits() == 0 && opened >= 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(contender.try_lock().is_err());
        worker.abort();
        assert!(matches!(worker.await, Err(error) if error.is_cancelled()));
        assert!(contender.try_lock().is_err());
        drop(disk_lock);
        tokio::time::timeout(Duration::from_secs(5), async {
            while capacity.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        contender.try_lock().unwrap();
        drop(contender);
        assert_ne!(
            crate::capture_keyring::rotate(&keys, &anchors, &spool).unwrap(),
            old
        );
    }

    #[sqlx::test]
    async fn cancelled_disk_write_cannot_be_resumed_as_a_complete_capture(pool: PgPool) {
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::Semaphore;

        let root = std::env::temp_dir().join(format!("capture-cancel-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let key_dir = root.join("keys");
        let anchors = root.join("anchors");
        std::fs::create_dir(&key_dir).unwrap();
        std::fs::create_dir(&anchors).unwrap();
        let master = create_key(&key_dir.join("master"), &anchors).unwrap();
        let capacity = Arc::new(Semaphore::new(1));
        let session = Arc::new(
            AsyncCaptureSession::create(
                &pool,
                CapturePaths {
                    spool: &root,
                    anchors: &anchors,
                },
                identity,
                &master,
                &[],
                crate::capture_spool::CaptureLimits::for_test(4096),
                capacity.clone(),
            )
            .await
            .unwrap(),
        );
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(".capture-capacity.lock"))
            .unwrap();
        lock.lock().unwrap();
        let copy = session.clone();
        let worker = tokio::spawn(async move {
            copy.save_requests(b"client".to_vec(), b"provider".to_vec())
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while capacity.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        drop(lock);
        tokio::time::timeout(Duration::from_secs(2), async {
            while capacity.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            session
                .save_response(CaptureLeg::ClientResponse, b"response".to_vec())
                .await
                .is_err()
        );
        assert!(session.finish().await.is_err());
        let path = root.join(format!("{}.capture", identity.capture));
        assert!(CaptureSpool::read(&path, identity, &master, 4096).is_err());
    }

    #[sqlx::test]
    async fn creating_a_spool_uses_bounded_background_capacity(pool: PgPool) {
        use std::process::{Command, Stdio};
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        use tokio::sync::Semaphore;

        let root = std::env::temp_dir().join(format!("capture-async-create-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let key_dir = root.join("keys");
        let anchors = root.join("anchors");
        std::fs::create_dir(&key_dir).unwrap();
        std::fs::create_dir(&anchors).unwrap();
        let master = create_key(&key_dir.join("master"), &anchors).unwrap();
        let lock_path = root.join(".capture-capacity.lock");
        std::fs::write(&lock_path, (4096u64 * 16).to_be_bytes()).unwrap();
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
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let capacity = Arc::new(Semaphore::new(1));
        let worker_pool = pool.clone();
        let worker_root = root.clone();
        let worker_anchors = anchors.clone();
        let worker_master = *master;
        let worker_capacity = capacity.clone();
        let worker = tokio::spawn(async move {
            AsyncCaptureSession::create(
                &worker_pool,
                CapturePaths {
                    spool: &worker_root,
                    anchors: &worker_anchors,
                },
                identity,
                &worker_master,
                &[],
                crate::capture_spool::CaptureLimits::for_test(4096),
                worker_capacity,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while capacity.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!worker.is_finished());
        let second = CaptureIdentity {
            tenant: identity.tenant,
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        assert!(
            AsyncCaptureSession::create(
                &pool,
                CapturePaths {
                    spool: &root,
                    anchors: &anchors
                },
                second,
                &master,
                &[],
                crate::capture_spool::CaptureLimits::for_test(4096),
                capacity.clone()
            )
            .await
            .is_err()
        );
        assert!(child.wait().unwrap().success());
        let session = worker.await.unwrap().unwrap();
        session
            .save_requests(b"client".to_vec(), b"provider".to_vec())
            .await
            .unwrap();
        session
            .save_response(CaptureLeg::ProviderResponse, b"provider".to_vec())
            .await
            .unwrap();
        session
            .save_response(CaptureLeg::ClientResponse, b"client".to_vec())
            .await
            .unwrap();
        session.finish().await.unwrap();
    }

    #[sqlx::test]
    async fn slow_disk_does_not_block_runtime_and_work_is_bounded(pool: PgPool) {
        use std::process::{Command, Stdio};
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        use tokio::sync::Semaphore;

        let root = std::env::temp_dir().join(format!("capture-async-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let session = CaptureSession::create(
            &pool,
            &root,
            identity,
            &[52u8; 32],
            &[],
            crate::capture_spool::CaptureLimits::for_test(4096),
        )
        .await
        .unwrap();
        let lock_path = root.join(".capture-capacity.lock");
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
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let session = Arc::new(AsyncCaptureSession::new(
            session,
            Arc::new(Semaphore::new(1)),
        ));
        let worker_session = session.clone();
        let worker = tokio::spawn(async move {
            worker_session
                .save_requests(b"client".to_vec(), b"provider".to_vec())
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!worker.is_finished());
        assert!(
            session
                .save_response(CaptureLeg::ProviderResponse, b"busy".to_vec())
                .await
                .is_err()
        );
        assert!(child.wait().unwrap().success());
        worker.await.unwrap().unwrap();
        session
            .save_response(CaptureLeg::ProviderResponse, b"provider".to_vec())
            .await
            .unwrap();
        session
            .save_response(CaptureLeg::ClientResponse, b"client".to_vec())
            .await
            .unwrap();
        session.finish().await.unwrap();
    }

    #[sqlx::test]
    async fn request_and_response_must_be_saved_before_they_can_continue(pool: PgPool) {
        let root = std::env::temp_dir().join(format!("capture-session-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let master = [51u8; 32];
        let mut session = CaptureSession::create(
            &pool,
            &root,
            identity,
            &master,
            &[b"secret-token"],
            crate::capture_spool::CaptureLimits::for_test(4096),
        )
        .await
        .unwrap();
        assert!(!session.may_send_upstream());
        assert!(
            session
                .save_response(CaptureLeg::ClientResponse, b"early")
                .is_err()
        );
        session
            .save_requests(b"client request", b"provider request")
            .unwrap();
        assert!(session.may_send_upstream());
        session
            .save_response(CaptureLeg::ProviderResponse, b"provider reply")
            .unwrap();
        session
            .save_response(CaptureLeg::ClientResponse, b"client reply")
            .unwrap();
        session.finish().unwrap();
        let path = root.join(format!("{}.capture", identity.capture));
        let records = CaptureSpool::read(&path, identity, &master, 4096).unwrap();
        assert_eq!(
            records,
            vec![
                b"client request".to_vec(),
                b"provider request".to_vec(),
                b"provider reply".to_vec(),
                b"client reply".to_vec()
            ]
        );

        let second = CaptureIdentity {
            tenant: identity.tenant,
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let mut failed = CaptureSession::create(
            &pool,
            &root,
            second,
            &master,
            &[b"secret-token"],
            crate::capture_spool::CaptureLimits::for_test(4096),
        )
        .await
        .unwrap();
        assert!(failed.save_requests(b"client", b"secret-token").is_err());
        assert!(!failed.may_send_upstream());
        assert!(failed.finish().is_err());

        let third = CaptureIdentity {
            tenant: identity.tenant,
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let mut full = CaptureSession::create(
            &pool,
            &root,
            third,
            &master,
            &[],
            crate::capture_spool::CaptureLimits::for_test(4096),
        )
        .await
        .unwrap();
        full.save_requests(b"client", b"provider").unwrap();
        assert!(
            full.save_response(CaptureLeg::ProviderResponse, &vec![7; 4096])
                .is_err()
        );
        assert!(!full.may_send_upstream());
        assert!(
            full.save_response(CaptureLeg::ClientResponse, b"must not forward")
                .is_err()
        );
        assert!(full.finish().is_err());
    }
}
