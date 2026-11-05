use std::sync::Arc;

use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::held_token::{PersonalTokens, TokenLock};

pub(crate) struct CredentialLease {
    files: Vec<TokenLock>,
    transaction: Option<Transaction<'static, Postgres>>,
}

impl From<TokenLock> for CredentialLease {
    fn from(file: TokenLock) -> Self {
        Self {
            files: vec![file],
            transaction: None,
        }
    }
}

impl Drop for CredentialLease {
    fn drop(&mut self) {
        self.transaction.take();
        self.files.clear();
    }
}

pub(crate) async fn select(
    pool: &PgPool,
    store: &PersonalTokens,
    tenant: Uuid,
    user: Uuid,
    provider: &str,
    kind: &str,
) -> Result<(Arc<str>, CredentialLease), Box<dyn std::error::Error + Send + Sync>> {
    let (personal, personal_lock) = store.read_locked(tenant, user).await?;
    if let Some(token) = personal {
        return Ok((token, personal_lock.into()));
    }
    let member_lock = store.lock_membership(tenant, user, true).await?;
    let mut transaction = pool.begin().await?;
    sqlx::query_file!("queries/credential_timeouts.sql")
        .fetch_one(&mut *transaction)
        .await?;
    let rows = sqlx::query_file!(
        "queries/group_credential_select.sql",
        tenant,
        user,
        provider,
        kind
    )
    .fetch_all(&mut *transaction)
    .await?;
    let [binding] = rows.as_slice() else {
        return Err(
            std::io::Error::other("provider credential binding is missing or ambiguous").into(),
        );
    };
    let (token, group_lock) = store
        .group_store(provider)
        .read_locked(tenant, binding.group_id)
        .await?;
    let token =
        token.ok_or_else(|| std::io::Error::other("group provider credential is missing"))?;
    if Sha256::digest(token.as_bytes()).as_slice() != binding.digest {
        return Err(
            std::io::Error::other("group provider credential does not match its binding").into(),
        );
    }
    Ok((
        token,
        CredentialLease {
            files: vec![personal_lock, member_lock, group_lock],
            transaction: Some(transaction),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test]
    async fn membership_fence_survives_database_backend_loss(pool: PgPool) {
        sqlx::raw_sql(include_str!("../tests/group_credentials_fixture.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let root = std::env::temp_dir().join(format!("group-backend-loss-{}", Uuid::new_v4()));
        let store = PersonalTokens::new(root.clone());
        let tenant = Uuid::from_u128(0x100);
        let user = Uuid::from_u128(1);
        let group = Uuid::from_u128(0x10);
        let provider = "anthropic-test";
        let token = "sk-ant-oat01-group";
        store
            .group_store(provider)
            .replace(tenant, group, token)
            .await
            .unwrap();
        let digest = store.digest(token).unwrap();
        sqlx::query_file!(
            "queries/admin/group_credential_set.sql",
            tenant,
            group,
            provider,
            "subscription",
            digest
        )
        .execute(&pool)
        .await
        .unwrap();
        let (_, mut lease) = select(&pool, &store, tenant, user, provider, "subscription")
            .await
            .unwrap();
        let backend = sqlx::query_file_scalar!("tests/current_backend_pid.sql")
            .fetch_one(&mut **lease.transaction.as_mut().unwrap())
            .await
            .unwrap();
        assert!(
            sqlx::query_file_scalar!("tests/terminate_lock_connection.sql", backend)
                .fetch_one(&pool)
                .await
                .unwrap()
        );
        assert!(store.lock_membership(tenant, user, false).await.is_err());
        drop(lease);
        let lock = store.lock_membership(tenant, user, false).await.unwrap();
        drop(lock);
        std::fs::remove_dir_all(root).unwrap();
    }
}
