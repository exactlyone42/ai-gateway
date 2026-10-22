use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row as _;
use uuid::Uuid;

use super::{
    AdminContext, AdminError, AdminRole, AdminService, AuditEntry, ListQuery, ResourcePage,
    append_audit, validate_name,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminAccountCreate {
    pub display_name: String,
    pub credential_name: String,
    pub role: AdminRole,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminAccountPatch {
    pub version: i64,
    pub display_name: Option<String>,
    pub role: Option<AdminRole>,
    pub status: Option<String>,
}

#[derive(Serialize)]
pub struct AdminAccountSecretOutput {
    pub account: Value,
    pub login_token: String,
}

impl AdminService {
    pub async fn list_admin_accounts(
        &self,
        context: &AdminContext,
        query: ListQuery,
    ) -> Result<ResourcePage, AdminError> {
        let limit = query.limit.unwrap_or(50);
        if limit == 0 || limit > 100 {
            return Err(AdminError::InvalidRequest);
        }
        let rows = sqlx::query(
            r#"
            SELECT id, to_jsonb(admin_accounts) AS account
            FROM admin_accounts
            WHERE tenant_id = $1 AND id > $2
            ORDER BY id LIMIT $3
            "#,
        )
        .bind(context.tenant_id)
        .bind(query.after.unwrap_or(Uuid::nil()))
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await?;
        let has_more = rows.len() > usize::from(limit);
        let mut items = Vec::with_capacity(usize::from(limit));
        let mut last_id = None;
        for row in rows.into_iter().take(usize::from(limit)) {
            last_id = Some(row.try_get::<Uuid, _>("id")?);
            items.push(row.try_get("account")?);
        }
        Ok(ResourcePage {
            items,
            next_after: has_more.then_some(last_id).flatten(),
        })
    }

    pub async fn create_admin_account(
        &self,
        context: &AdminContext,
        input: AdminAccountCreate,
        source_ip: IpAddr,
        peer_ip: IpAddr,
        request_id: &str,
    ) -> Result<AdminAccountSecretOutput, AdminError> {
        validate_name(&input.display_name)?;
        validate_name(&input.credential_name)?;
        let account_id = Uuid::new_v4();
        let credential_id = Uuid::new_v4();
        let material = self.generate("admin", context.tenant_id, b"admin-login\0")?;
        let mut transaction = self.pool.begin().await?;
        let account: Value = sqlx::query_scalar(
            r#"
            INSERT INTO admin_accounts (id, tenant_id, display_name, role)
            VALUES ($1, $2, $3, $4)
            RETURNING to_jsonb(admin_accounts)
            "#,
        )
        .bind(account_id)
        .bind(context.tenant_id)
        .bind(input.display_name)
        .bind(input.role.as_str())
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO admin_login_credentials (
                id, tenant_id, admin_account_id, display_name, secret_hash,
                hash_key_id, key_prefix, fingerprint
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(credential_id)
        .bind(context.tenant_id)
        .bind(account_id)
        .bind(input.credential_name)
        .bind(material.secret_hash.as_slice())
        .bind(&material.hash_key_id)
        .bind(&material.key_prefix)
        .bind(material.fingerprint.as_slice())
        .execute(&mut *transaction)
        .await?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: "admin.account.create",
                target_type: "admin_account",
                target_id: Some(account_id),
                before: None,
                after: Some(account.clone()),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(AdminAccountSecretOutput {
            account,
            login_token: material.plaintext,
        })
    }

    pub async fn update_admin_account(
        &self,
        context: &AdminContext,
        id: Uuid,
        input: AdminAccountPatch,
        source_ip: IpAddr,
        peer_ip: IpAddr,
        request_id: &str,
    ) -> Result<Value, AdminError> {
        if input.version <= 0
            || (input.display_name.is_none() && input.role.is_none() && input.status.is_none())
        {
            return Err(AdminError::InvalidRequest);
        }
        if let Some(name) = input.display_name.as_deref() {
            validate_name(name)?;
        }
        if input
            .status
            .as_deref()
            .is_some_and(|status| !matches!(status, "active" | "disabled"))
        {
            return Err(AdminError::InvalidRequest);
        }
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 824161))")
            .bind(context.tenant_id)
            .execute(&mut *transaction)
            .await?;
        let current = sqlx::query(
            r#"
            SELECT role, status, to_jsonb(admin_accounts) AS account
            FROM admin_accounts
            WHERE tenant_id = $1 AND id = $2
            FOR UPDATE
            "#,
        )
        .bind(context.tenant_id)
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(AdminError::NotFound)?;
        let current_role: String = current.try_get("role")?;
        let current_status: String = current.try_get("status")?;
        let would_remove_admin = current_role == "admin"
            && current_status == "active"
            && (input.role.is_some_and(|role| role != AdminRole::Admin)
                || input.status.as_deref() == Some("disabled"));
        if would_remove_admin {
            let active_admins: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM admin_accounts WHERE tenant_id = $1 AND role = 'admin' AND status = 'active'",
            )
            .bind(context.tenant_id)
            .fetch_one(&mut *transaction)
            .await?;
            if active_admins <= 1 {
                return Err(AdminError::Conflict);
            }
        }
        let before: Value = current.try_get("account")?;
        let after: Value = sqlx::query_scalar(
            r#"
            UPDATE admin_accounts
            SET display_name = COALESCE($4, display_name),
                role = COALESCE($5, role), status = COALESCE($6, status),
                version = version + 1, updated_at = now()
            WHERE tenant_id = $1 AND id = $2 AND version = $3
            RETURNING to_jsonb(admin_accounts)
            "#,
        )
        .bind(context.tenant_id)
        .bind(id)
        .bind(input.version)
        .bind(input.display_name)
        .bind(input.role.map(AdminRole::as_str))
        .bind(input.status)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(AdminError::Conflict)?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: "admin.account.rbac_update",
                target_type: "admin_account",
                target_id: Some(id),
                before: Some(before),
                after: Some(after.clone()),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(after)
    }
}

#[cfg(test)]
mod tests {
    use std::{net::IpAddr, str::FromStr as _, time::Duration};

    use sqlx::PgPool;
    use uuid::Uuid;

    use super::{AdminAccountCreate, AdminAccountPatch};
    use crate::{
        admin::{AdminError, AdminRole, AdminService, AdminServiceConfig, BootstrapInput},
        identity::CredentialService,
    };

    const BOOTSTRAP: &str = "account-test-bootstrap-token-at-least-32-bytes";

    fn service(pool: PgPool, tenant_id: Uuid) -> AdminService {
        let hash_key = b"account-test-credential-hash-key-at-least-32".to_vec();
        let credentials = CredentialService::new(
            pool.clone(),
            "v1".to_owned(),
            hash_key.clone(),
            Vec::new(),
            "test".to_owned(),
            16,
            Duration::from_secs(60),
        );
        AdminService::new(
            pool,
            credentials,
            AdminServiceConfig {
                hash_key,
                hash_key_id: "v1".to_owned(),
                retiring_hash_keys: Vec::new(),
                bootstrap_token: Some(BOOTSTRAP.to_owned()),
                bootstrap_tenant_id: Some(tenant_id),
                session_ttl: Duration::from_secs(900),
            },
        )
    }

    #[sqlx::test]
    async fn last_admin_is_protected_and_rbac_change_invalidates_the_old_session(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let service = service(pool, tenant_id);
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let owner = service
            .bootstrap(
                BOOTSTRAP,
                BootstrapInput {
                    display_name: "owner".to_owned(),
                    credential_name: "owner login".to_owned(),
                },
                source,
                source,
                "bootstrap",
            )
            .await
            .unwrap();
        let owner_session = service
            .login(&owner.login_token, source, source, "owner-login")
            .await
            .unwrap();
        let owner_context = service
            .resolve_session(&owner_session.access_token)
            .await
            .unwrap();
        assert!(matches!(
            service
                .update_admin_account(
                    &owner_context,
                    owner.account_id,
                    AdminAccountPatch {
                        version: 1,
                        display_name: None,
                        role: Some(AdminRole::Viewer),
                        status: None,
                    },
                    source,
                    source,
                    "remove-last-admin",
                )
                .await,
            Err(AdminError::Conflict)
        ));

        service
            .create_admin_account(
                &owner_context,
                AdminAccountCreate {
                    display_name: "second owner".to_owned(),
                    credential_name: "second owner login".to_owned(),
                    role: AdminRole::Admin,
                },
                source,
                source,
                "create-second-admin",
            )
            .await
            .unwrap();
        service
            .update_admin_account(
                &owner_context,
                owner.account_id,
                AdminAccountPatch {
                    version: 1,
                    display_name: None,
                    role: Some(AdminRole::Viewer),
                    status: None,
                },
                source,
                source,
                "demote-owner",
            )
            .await
            .unwrap();
        assert!(matches!(
            service.resolve_session(&owner_session.access_token).await,
            Err(AdminError::Unauthorized)
        ));
    }

    #[sqlx::test]
    async fn concurrent_demotions_cannot_remove_every_admin(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO admin_accounts (id, tenant_id, display_name, role) VALUES ($1, $3, 'first', 'admin'), ($2, $3, 'second', 'admin')",
        )
        .bind(first)
        .bind(second)
        .bind(tenant_id)
        .execute(&pool)
        .await
        .unwrap();
        let service = service(pool.clone(), tenant_id);
        let context = crate::admin::AdminContext {
            tenant_id,
            account_id: first,
            session_id: Uuid::new_v4(),
            role: AdminRole::Admin,
        };
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let first_update = service.update_admin_account(
            &context,
            first,
            AdminAccountPatch {
                version: 1,
                display_name: None,
                role: Some(AdminRole::Viewer),
                status: None,
            },
            source,
            source,
            "concurrent-first",
        );
        let second_update = service.update_admin_account(
            &context,
            second,
            AdminAccountPatch {
                version: 1,
                display_name: None,
                role: Some(AdminRole::Viewer),
                status: None,
            },
            source,
            source,
            "concurrent-second",
        );
        let (first_result, second_result) = tokio::join!(first_update, second_update);
        assert_eq!(
            usize::from(first_result.is_ok()) + usize::from(second_result.is_ok()),
            1
        );
        assert!(matches!(
            first_result.err().or_else(|| second_result.err()),
            Some(AdminError::Conflict)
        ));
        let active_admins: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM admin_accounts WHERE tenant_id = $1 AND role = 'admin' AND status = 'active'",
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(active_admins, 1);
    }
}
