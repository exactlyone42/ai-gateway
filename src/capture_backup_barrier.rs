use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
#[error("capture backup coordination unavailable")]
pub(crate) struct CaptureBarrierError;

pub(crate) struct CaptureBarrierLease {
    _file: std::fs::File,
}

fn open(root: &Path) -> Result<std::fs::File, CaptureBarrierError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(root.join(".capture-backup.lock"))
        .map_err(|_| CaptureBarrierError)?;
    let meta = file.metadata().map_err(|_| CaptureBarrierError)?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 {
        return Err(CaptureBarrierError);
    }
    Ok(file)
}

pub(crate) fn try_shared(root: &Path) -> Result<CaptureBarrierLease, CaptureBarrierError> {
    let file = open(root)?;
    file.try_lock_shared().map_err(|_| CaptureBarrierError)?;
    Ok(CaptureBarrierLease { _file: file })
}

#[cfg(test)]
pub(crate) fn try_exclusive(root: &Path) -> Result<CaptureBarrierLease, CaptureBarrierError> {
    let file = open(root)?;
    file.try_lock().map_err(|_| CaptureBarrierError)?;
    Ok(CaptureBarrierLease { _file: file })
}

#[cfg(test)]
mod tests {
    use super::try_exclusive;
    use crate::capture_archive::{delete_coordinated, recover_and_import_keyring};
    use crate::capture_crypto::{CaptureIdentity, CaptureLeg};
    use crate::capture_keyring::rotate;
    use crate::capture_maintenance::CaptureMaintenance;
    use crate::capture_session::{AsyncCaptureSession, CapturePaths};
    use crate::capture_spool::CaptureLimits;
    use sqlx::PgPool;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    use uuid::Uuid;

    #[sqlx::test]
    async fn backup_excludes_new_sessions_and_maintenance_until_release(pool: PgPool) {
        let base =
            std::path::Path::new("/tmp").join(format!("capture-backup-lock-{}", Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let spool = base.join("spool");
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        for dir in [&spool, &keys, &anchors] {
            std::fs::create_dir(dir).unwrap();
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let current = rotate(&keys, &anchors, &spool).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let credential = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
            .bind(identity.user)
            .bind(identity.tenant)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO credentials (id, tenant_id, type, display_name, secret_hash, key_prefix, fingerprint, user_id, hash_key_id) VALUES ($1, $2, 'client_api_key', 'test', $3, 'test', $3, $4, 'v1')")
            .bind(credential).bind(identity.tenant).bind(vec![4u8; 32]).bind(identity.user).execute(&pool).await.unwrap();
        let limits = CaptureLimits::for_test(4096);
        let capacity = Arc::new(Semaphore::new(1));
        let backup = try_exclusive(&spool).unwrap();
        assert!(rotate(&keys, &anchors, &spool).is_err());
        assert!(
            crate::capture_keyring::current(&keys).unwrap().as_ref()
                == crate::capture_master_key::load_versioned_key(&keys, &current)
                    .unwrap()
                    .as_ref()
        );
        assert!(
            AsyncCaptureSession::create_from_keyring(
                &pool,
                CapturePaths {
                    spool: &spool,
                    anchors: &anchors
                },
                &keys,
                identity,
                &[],
                limits,
                capacity.clone()
            )
            .await
            .is_err()
        );
        assert!(!spool.join(format!("{}.capture", identity.capture)).exists());
        let uses: i64 = sqlx::query_scalar("SELECT count(*) FROM capture_master_key_usage")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(uses, 0);
        let maintenance =
            CaptureMaintenance::new(spool.clone(), keys.clone(), limits, capacity.clone());
        assert!(maintenance.run_once(&pool).await.is_err());
        drop(backup);
        let session = AsyncCaptureSession::create_from_keyring(
            &pool,
            CapturePaths {
                spool: &spool,
                anchors: &anchors,
            },
            &keys,
            identity,
            &[],
            limits,
            capacity.clone(),
        )
        .await
        .unwrap();
        assert!(try_exclusive(&spool).is_err());
        session
            .save_requests(b"client".to_vec(), b"provider".to_vec())
            .await
            .unwrap();
        session
            .save_response(CaptureLeg::ProviderResponse, b"reply".to_vec())
            .await
            .unwrap();
        session
            .save_response(CaptureLeg::ClientResponse, b"reply".to_vec())
            .await
            .unwrap();
        session.finish().await.unwrap();
        let path = spool.join(format!("{}.capture", identity.capture));
        recover_and_import_keyring(&pool, &path, &keys, limits.per_file, capacity.clone())
            .await
            .unwrap();
        assert!(
            delete_coordinated(&pool, &spool, identity, Uuid::new_v4(), capacity.clone())
                .await
                .is_err()
        );
        let backup = try_exclusive(&spool).unwrap();
        assert!(
            delete_coordinated(&pool, &spool, identity, credential, capacity.clone())
                .await
                .is_err()
        );
        let state: String = sqlx::query_scalar(
            "SELECT state FROM capture_records WHERE tenant_id = $1 AND capture_id = $2",
        )
        .bind(identity.tenant)
        .bind(identity.capture)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, "complete");
        drop(backup);
        assert!(
            delete_coordinated(&pool, &spool, identity, credential, capacity)
                .await
                .unwrap()
        );
    }
}
