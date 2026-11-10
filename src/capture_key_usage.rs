use sha2::{Digest, Sha256};
use sqlx::PgPool;

#[derive(Debug, thiserror::Error)]
pub(crate) enum CaptureKeyUsageError {
    #[error("capture master key usage unavailable")]
    Unavailable,
    #[error("capture master key has reached its usage limit")]
    Exhausted,
}

pub(crate) struct WrapPermit {
    fingerprint: [u8; 32],
    count: i64,
}

impl WrapPermit {
    pub(crate) fn count(&self) -> i64 {
        self.count
    }
    pub(crate) fn matches(&self, master: &[u8; 32]) -> bool {
        self.fingerprint == <[u8; 32]>::from(Sha256::digest(master))
    }

    #[cfg(test)]
    pub(crate) fn for_test(master: &[u8; 32]) -> Self {
        Self {
            fingerprint: Sha256::digest(master).into(),
            count: 0,
        }
    }
}

pub(crate) async fn reserve_master_wrap(
    pool: &PgPool,
    master: &[u8; 32],
) -> Result<WrapPermit, CaptureKeyUsageError> {
    let fingerprint = Sha256::digest(master);
    let count: i64 = sqlx::query_scalar(
        "INSERT INTO capture_master_key_usage (key_fingerprint, wrap_count) VALUES ($1, 1) \
         ON CONFLICT (key_fingerprint) DO UPDATE SET wrap_count = capture_master_key_usage.wrap_count + 1 \
         WHERE capture_master_key_usage.wrap_count < 1048576 RETURNING wrap_count",
    )
    .bind(fingerprint.as_slice())
    .fetch_optional(pool)
    .await
    .map_err(|_| CaptureKeyUsageError::Unavailable)?
    .ok_or(CaptureKeyUsageError::Exhausted)?;
    Ok(WrapPermit {
        fingerprint: fingerprint.into(),
        count,
    })
}

#[cfg(test)]
mod tests {
    use super::{CaptureKeyUsageError, reserve_master_wrap};
    use sha2::{Digest, Sha256};
    use sqlx::PgPool;

    #[sqlx::test]
    async fn reserved_permit_is_required_for_spool_encryption(pool: PgPool) {
        use crate::capture_crypto::{CaptureCipher, CaptureIdentity, CaptureLeg};
        use crate::capture_spool::CaptureSpool;
        use uuid::Uuid;

        let root = std::env::temp_dir().join(format!("capture-permit-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let master = [21u8; 32];
        let permit = reserve_master_wrap(&pool, &master).await.unwrap();
        assert!(CaptureCipher::create(&[22u8; 32], identity, permit).is_err());
        assert!(!root.join(format!("{}.capture", identity.capture)).exists());
        let mut spool = CaptureSpool::create(
            &pool,
            &root,
            identity,
            &master,
            &[],
            crate::capture_spool::CaptureLimits::for_test(4096),
        )
        .await
        .unwrap();
        spool
            .append(CaptureLeg::ClientRequest, 0, b"secret")
            .unwrap();
        let path = spool.path().to_owned();
        spool.finish().unwrap();
        assert_eq!(
            CaptureSpool::read(&path, identity, &master, 4096).unwrap(),
            vec![b"secret".to_vec()]
        );
        let fingerprint = Sha256::digest(master);
        let count: i64 = sqlx::query_scalar(
            "SELECT wrap_count FROM capture_master_key_usage WHERE key_fingerprint = $1",
        )
        .bind(fingerprint.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 2);
    }

    #[sqlx::test]
    async fn closed_database_cannot_issue_a_permit(pool: PgPool) {
        use crate::capture_crypto::CaptureIdentity;
        use crate::capture_spool::CaptureSpool;
        use uuid::Uuid;

        let root = std::env::temp_dir().join(format!("capture-offline-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        pool.close().await;
        assert!(matches!(
            reserve_master_wrap(&pool, &[23u8; 32]).await,
            Err(CaptureKeyUsageError::Unavailable)
        ));
        assert!(
            CaptureSpool::create(
                &pool,
                &root,
                identity,
                &[23u8; 32],
                &[],
                crate::capture_spool::CaptureLimits::for_test(4096)
            )
            .await
            .is_err()
        );
        assert!(!root.join(format!("{}.capture", identity.capture)).exists());
    }

    #[sqlx::test]
    async fn reserve_uses_master_key_fingerprint_and_fails_after_limit(pool: PgPool) {
        let master = [31u8; 32];
        assert_eq!(reserve_master_wrap(&pool, &master).await.unwrap().count, 1);
        assert_eq!(reserve_master_wrap(&pool, &master).await.unwrap().count, 2);
        let fingerprint = Sha256::digest(master);
        sqlx::query(
            "UPDATE capture_master_key_usage SET wrap_count = 1048576 WHERE key_fingerprint = $1",
        )
        .bind(fingerprint.as_slice())
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            reserve_master_wrap(&pool, &master).await,
            Err(CaptureKeyUsageError::Exhausted)
        ));
        assert_eq!(
            reserve_master_wrap(&pool, &[32u8; 32]).await.unwrap().count,
            1
        );
    }
}
