use crate::capture_archive::{
    CaptureArchiveError, recover_and_import_keyring, remove_archived_spool, remove_deleted_spool,
};
use crate::capture_spool::CaptureLimits;
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

pub(crate) struct RecoveryScanner {
    entries: Arc<Mutex<Option<std::fs::ReadDir>>>,
    capacity: Arc<Semaphore>,
}

pub(crate) struct RecoveryReport {
    pub examined: usize,
    pub imported: usize,
    pub failed: usize,
    pub finished: bool,
}

impl RecoveryScanner {
    pub(crate) async fn open(
        root: &Path,
        capacity: Arc<Semaphore>,
    ) -> Result<Self, CaptureArchiveError> {
        let permit = capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| CaptureArchiveError)?;
        let root = root.to_owned();
        let entries = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            std::fs::read_dir(root).map_err(|_| CaptureArchiveError)
        })
        .await
        .map_err(|_| CaptureArchiveError)??;
        Ok(Self {
            entries: Arc::new(Mutex::new(Some(entries))),
            capacity,
        })
    }

    pub(crate) async fn scan_batch(
        &self,
        pool: &PgPool,
        key_root: &Path,
        limits: CaptureLimits,
        limit: usize,
    ) -> Result<RecoveryReport, CaptureArchiveError> {
        if !(1..=32).contains(&limit) {
            return Err(CaptureArchiveError);
        }
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| CaptureArchiveError)?;
        let entries = self.entries.clone();
        let (paths, examined, finished) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut cursor = entries.lock().map_err(|_| CaptureArchiveError)?;
            let mut paths: Vec<PathBuf> = Vec::new();
            let mut examined = 0;
            while examined < limit {
                let next = match cursor.as_mut() {
                    Some(iter) => iter.next(),
                    None => None,
                };
                match next {
                    None => {
                        *cursor = None;
                        break;
                    }
                    Some(Err(_)) => return Err(CaptureArchiveError),
                    Some(Ok(entry)) => {
                        examined += 1;
                        if entry
                            .path()
                            .extension()
                            .is_some_and(|extension| extension == "capture")
                        {
                            paths.push(entry.path());
                        }
                    }
                }
            }
            Ok::<_, CaptureArchiveError>((paths, examined, cursor.is_none()))
        })
        .await
        .map_err(|_| CaptureArchiveError)??;
        let mut imported = 0;
        let mut failed = 0;
        for path in paths {
            match recover_and_import_keyring(
                pool,
                &path,
                key_root,
                limits.per_file,
                self.capacity.clone(),
            )
            .await
            {
                Ok(complete) => {
                    imported += 1;
                    if complete
                        && remove_archived_spool(
                            pool,
                            &path,
                            key_root,
                            limits,
                            self.capacity.clone(),
                        )
                        .await
                        .is_err()
                    {
                        failed += 1;
                    }
                }
                Err(_) => {
                    if remove_deleted_spool(pool, &path, key_root, limits, self.capacity.clone())
                        .await
                        .is_err()
                    {
                        failed += 1;
                    }
                }
            }
        }
        Ok(RecoveryReport {
            examined,
            imported,
            failed,
            finished,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::RecoveryScanner;
    use crate::capture_anchor::{create_versioned_key, reserve_anchored};
    use crate::capture_archive::delete_batch;
    use crate::capture_crypto::{CaptureIdentity, CaptureLeg};
    use crate::capture_spool::{CaptureLimits, CaptureSpool};
    use sqlx::PgPool;
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    use uuid::Uuid;

    #[sqlx::test]
    async fn scan_continues_in_bounded_batches_and_keeps_incomplete_files(pool: PgPool) {
        let base = std::env::temp_dir().join(format!("capture-scan-{}", Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        let root = base.join("spool");
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&keys).unwrap();
        std::fs::create_dir(&anchors).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let incomplete = CaptureIdentity {
            capture: Uuid::new_v4(),
            ..identity
        };
        let master = create_versioned_key(&keys, &anchors).unwrap();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(identity.user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        let permit = reserve_anchored(&pool, &anchors, &master).await.unwrap();
        let mut completed = CaptureSpool::create_with_permit(
            &root,
            identity,
            &master,
            &[],
            permit,
            CaptureLimits::for_test(4096),
        )
        .unwrap();
        completed
            .append(CaptureLeg::ClientRequest, 0, b"safe")
            .unwrap();
        let complete_path = completed.path().to_owned();
        completed.finish().unwrap();
        let permit = reserve_anchored(&pool, &anchors, &master).await.unwrap();
        let unfinished = CaptureSpool::create_with_permit(
            &root,
            incomplete,
            &master,
            &[],
            permit,
            CaptureLimits::for_test(4096),
        )
        .unwrap();
        let unfinished_path = unfinished.path().to_owned();
        let capacity = Arc::new(Semaphore::new(1));
        let scanner = RecoveryScanner::open(&root, capacity.clone())
            .await
            .unwrap();
        let _new_master = create_versioned_key(&keys, &anchors).unwrap();
        assert!(
            scanner
                .scan_batch(&pool, &keys, CaptureLimits::for_test(4096), 0)
                .await
                .is_err()
        );
        let mut examined = 0;
        let mut imported = 0;
        let mut failed = 0;
        loop {
            let batch = scanner
                .scan_batch(&pool, &keys, CaptureLimits::for_test(4096), 2)
                .await
                .unwrap();
            assert!(batch.examined <= 2);
            examined += batch.examined;
            imported += batch.imported;
            failed += batch.failed;
            if batch.finished {
                break;
            }
        }
        assert_eq!((examined, imported, failed), (3, 1, 1));
        assert!(!complete_path.exists());
        assert!(unfinished_path.exists());
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM capture_records WHERE tenant_id = $1")
                .bind(identity.tenant)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        drop(unfinished);
        let scanner = RecoveryScanner::open(&root, capacity.clone())
            .await
            .unwrap();
        let batch = scanner
            .scan_batch(&pool, &keys, CaptureLimits::for_test(4096), 32)
            .await
            .unwrap();
        assert_eq!((batch.imported, batch.failed, batch.finished), (1, 0, true));
        let state: String = sqlx::query_scalar(
            "SELECT state FROM capture_records WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(incomplete.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, "incomplete");
        assert!(
            delete_batch(&pool, identity.tenant, identity.user, incomplete.capture)
                .await
                .unwrap()
        );
        let scanner = RecoveryScanner::open(&root, capacity).await.unwrap();
        let batch = scanner
            .scan_batch(&pool, &keys, CaptureLimits::for_test(4096), 32)
            .await
            .unwrap();
        assert_eq!((batch.imported, batch.failed), (0, 0));
        assert!(!unfinished_path.exists());
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 2);
    }
}
