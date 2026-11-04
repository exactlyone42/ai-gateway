use crate::capture_archive::{CaptureArchiveError, expire_batch, replay_erasure};
use crate::capture_backup_barrier::try_shared;
use crate::capture_recovery::RecoveryScanner;
use crate::capture_spool::CaptureLimits;
use sqlx::PgPool;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore, watch};

const MAINTENANCE_LOCK: i64 = 0x4150474341505452;

pub(crate) struct CaptureMaintenance {
    spool: PathBuf,
    keys: PathBuf,
    limits: CaptureLimits,
    capacity: Arc<Semaphore>,
    scanner: Mutex<Option<RecoveryScanner>>,
    erasure: Arc<crate::capture_erasure::ActiveErasure>,
    replayed: std::sync::atomic::AtomicUsize,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MaintenanceResult {
    Busy,
    Completed {
        examined: usize,
        imported: usize,
        failed: usize,
        expiration_batches: usize,
        recovery_complete: bool,
    },
}

impl CaptureMaintenance {
    pub(crate) fn new(
        spool: PathBuf,
        keys: PathBuf,
        limits: CaptureLimits,
        erasure: Arc<crate::capture_erasure::ActiveErasure>,
        replayed: usize,
    ) -> Self {
        Self {
            spool,
            keys,
            limits,
            capacity: Arc::new(Semaphore::new(1)),
            scanner: Mutex::new(None),
            erasure,
            replayed: std::sync::atomic::AtomicUsize::new(replayed),
        }
    }

    pub(crate) async fn run_once(
        &self,
        pool: &PgPool,
    ) -> Result<MaintenanceResult, CaptureArchiveError> {
        if pool.options().get_max_connections() < 2 {
            return Err(CaptureArchiveError);
        }
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| CaptureArchiveError)?;
        let root = self.spool.clone();
        let backup_guard = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            try_shared(&root).map_err(|_| CaptureArchiveError)
        })
        .await
        .map_err(|_| CaptureArchiveError)??;
        let Ok(mut scanner) = self.scanner.try_lock() else {
            return Ok(MaintenanceResult::Busy);
        };
        let mut tx = pool.begin().await.map_err(|_| CaptureArchiveError)?;
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(MAINTENANCE_LOCK)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| CaptureArchiveError)?;
        if !locked {
            return Ok(MaintenanceResult::Busy);
        }
        let start = self.replayed.load(std::sync::atomic::Ordering::Acquire);
        let replayed = replay_erasure(
            pool,
            self.erasure.clone(),
            &self.spool,
            &self.keys,
            self.limits,
            start,
        )
        .await?;
        self.replayed
            .store(replayed, std::sync::atomic::Ordering::Release);
        if scanner.is_none() {
            *scanner = Some(RecoveryScanner::open(&self.spool, self.capacity.clone()).await?);
        }
        let report = scanner
            .as_ref()
            .ok_or(CaptureArchiveError)?
            .scan_batch(pool, &self.keys, self.limits, 32)
            .await?;
        if report.failed > 0 {
            tracing::warn!(error.type = "capture_recovery_deferred", failed.count = report.failed);
        }
        if report.finished {
            *scanner = None;
        }
        let mut expiration_batches = 0;
        while expiration_batches < 32 && expire_batch(pool, self.erasure.clone()).await? {
            expiration_batches += 1;
        }
        tx.commit().await.map_err(|_| CaptureArchiveError)?;
        drop(backup_guard);
        Ok(MaintenanceResult::Completed {
            examined: report.examined,
            imported: report.imported,
            failed: report.failed,
            expiration_batches,
            recovery_complete: report.finished,
        })
    }

    pub(crate) async fn prepare(&self, pool: &PgPool) -> Result<(), CaptureArchiveError> {
        let mut last_report = std::time::Instant::now();
        let mut rescan_after_expiration = false;
        let mut reported_waiting = false;
        loop {
            if let MaintenanceResult::Completed {
                failed,
                recovery_complete,
                expiration_batches,
                ..
            } = self.run_once(pool).await?
            {
                if failed > 0 {
                    return Err(CaptureArchiveError);
                }
                rescan_after_expiration |= expiration_batches > 0;
                if recovery_complete
                    && !sqlx::query_file_scalar!("queries/capture_expire_pending.sql")
                        .fetch_one(pool)
                        .await
                        .map_err(|_| CaptureArchiveError)?
                {
                    if !rescan_after_expiration {
                        return Ok(());
                    }
                    rescan_after_expiration = false;
                }
            }
            if !reported_waiting || last_report.elapsed() >= Duration::from_secs(30) {
                tracing::info!(
                    event.name = "capture_startup_cleanup_waiting",
                    "waiting for capture cleanup"
                );
                last_report = std::time::Instant::now();
                reported_waiting = true;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub(crate) async fn run(
        &self,
        pool: &PgPool,
        interval: Duration,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), CaptureArchiveError> {
        if interval.is_zero() {
            return Err(CaptureArchiveError);
        }
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            if let Err(error) = self.run_once(pool).await {
                tracing::error!(error.type = "capture_maintenance_failed", exception.message = %error);
            }
            tokio::select! {
                _ = tokio::time::sleep(interval) => {},
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CaptureMaintenance, MAINTENANCE_LOCK, MaintenanceResult};
    use crate::capture_anchor::{create_versioned_key, reserve_anchored};
    use crate::capture_crypto::{CaptureIdentity, CaptureLeg};
    use crate::capture_spool::{CaptureLimits, CaptureSpool};
    use sqlx::PgPool;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::watch;
    use uuid::Uuid;

    #[sqlx::test]
    async fn occupied_connection_stalls_maintenance_in_two_connection_pool(pool: PgPool) {
        let erasure = crate::capture_erasure::TestErasure::new();
        let base = std::env::temp_dir().join(format!("capture-pool-{}", Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        let limited = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        let held = limited.acquire().await.unwrap();
        let maintenance = CaptureMaintenance::new(
            base.clone(),
            base.clone(),
            CaptureLimits::for_test(4096),
            erasure.active.clone(),
            0,
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(250), maintenance.run_once(&limited))
                .await
                .is_err()
        );
        drop(held);
        assert!(
            tokio::time::timeout(Duration::from_secs(3), maintenance.run_once(&limited))
                .await
                .unwrap()
                .is_ok()
        );
        limited.close().await;
        let larger = sqlx::postgres::PgPoolOptions::new()
            .max_connections(3)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        let held = larger.acquire().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), maintenance.run_once(&larger))
                .await
                .unwrap()
                .is_ok()
        );
        drop(held);
        larger.close().await;
        std::fs::remove_dir_all(base).unwrap();
    }

    #[sqlx::test]
    async fn maintenance_uses_one_database_lock_and_stops_on_request(pool: PgPool) {
        let erasure = crate::capture_erasure::TestErasure::new();
        let base = std::env::temp_dir().join(format!("capture-maintenance-{}", Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        let spool = base.join("spool");
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        std::fs::create_dir(&spool).unwrap();
        std::fs::create_dir(&keys).unwrap();
        std::fs::create_dir(&anchors).unwrap();
        let master = create_versioned_key(&keys, &anchors).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(identity.user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        let permit = reserve_anchored(&pool, &anchors, &master).await.unwrap();
        let mut file = CaptureSpool::create_with_permit(
            &spool,
            identity,
            &master,
            &[],
            permit,
            CaptureLimits::for_test(4096),
        )
        .unwrap();
        file.append(CaptureLeg::ClientRequest, 0, b"request")
            .unwrap();
        file.finish().unwrap();
        let maintenance = Arc::new(CaptureMaintenance::new(
            spool.clone(),
            keys,
            CaptureLimits::for_test(4096),
            erasure.active.clone(),
            0,
        ));
        let single = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), maintenance.run_once(&single))
                .await
                .unwrap()
                .is_err()
        );
        single.close().await;
        let mut guard = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(MAINTENANCE_LOCK)
            .execute(&mut *guard)
            .await
            .unwrap();
        assert_eq!(
            maintenance.run_once(&pool).await.unwrap(),
            MaintenanceResult::Busy
        );
        guard.commit().await.unwrap();
        assert_eq!(
            maintenance.run_once(&pool).await.unwrap(),
            MaintenanceResult::Completed {
                examined: 3,
                imported: 1,
                failed: 0,
                expiration_batches: 0,
                recovery_complete: true,
            }
        );
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM capture_records WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
        let expired = CaptureIdentity {
            capture: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis() - 2 * 86_400_000,
            retention_days: 1,
            ..identity
        };
        let permit = reserve_anchored(&pool, &anchors, &master).await.unwrap();
        let mut interrupted = CaptureSpool::create_with_permit(
            &spool,
            expired,
            &master,
            &[],
            permit,
            CaptureLimits::for_test(4096),
        )
        .unwrap();
        interrupted
            .append(CaptureLeg::ClientRequest, 0, b"expired")
            .unwrap();
        let expired_path = interrupted.path().to_owned();
        drop(interrupted);
        let result = maintenance.run_once(&pool).await.unwrap();
        assert!(matches!(
            result,
            MaintenanceResult::Completed {
                expiration_batches: 1,
                ..
            }
        ));
        assert!(expired_path.exists());
        maintenance.run_once(&pool).await.unwrap();
        assert!(!expired_path.exists());
        let state: String = sqlx::query_scalar(
            "SELECT state FROM capture_records WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(expired.tenant)
        .bind(expired.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, "deleted");
        let expired_before_start = CaptureIdentity {
            capture: Uuid::new_v4(),
            ..expired
        };
        let permit = reserve_anchored(&pool, &anchors, &master).await.unwrap();
        let mut unfinished = CaptureSpool::create_with_permit(
            &spool,
            expired_before_start,
            &master,
            &[],
            permit,
            CaptureLimits::for_test(4096),
        )
        .unwrap();
        unfinished
            .append(CaptureLeg::ClientRequest, 0, b"expired-before-start")
            .unwrap();
        let unfinished_path = unfinished.path().to_owned();
        drop(unfinished);
        maintenance.prepare(&pool).await.unwrap();
        assert!(!unfinished_path.exists());
        assert!(
            sqlx::query_file_scalar!(
                "queries/test_capture_deleted.sql",
                expired_before_start.tenant,
                expired_before_start.capture
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        );
        let second = CaptureIdentity {
            capture: Uuid::new_v4(),
            ..identity
        };
        let permit = reserve_anchored(&pool, &anchors, &master).await.unwrap();
        let mut later = CaptureSpool::create_with_permit(
            &spool,
            second,
            &master,
            &[],
            permit,
            CaptureLimits::for_test(4096),
        )
        .unwrap();
        later
            .append(CaptureLeg::ClientRequest, 0, b"later")
            .unwrap();
        later.finish().unwrap();
        let (sender, receiver) = watch::channel(false);
        let worker_pool = pool.clone();
        let worker_maintenance = maintenance.clone();
        let worker = tokio::spawn(async move {
            worker_maintenance
                .run(&worker_pool, Duration::from_secs(3600), receiver)
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let count: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM capture_records WHERE tenant_id = $1 AND capture_id = $2",
                )
                .bind(identity.tenant)
                .bind(second.capture)
                .fetch_one(&pool)
                .await
                .unwrap();
                if count == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        sender.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
