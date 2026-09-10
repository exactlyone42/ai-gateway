use std::{collections::HashMap, net::IpAddr, sync::Arc, time::Duration};

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac as _};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row as _, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::error::ErrorType;
use crate::identity::CredentialService;

mod accounts;
mod resources;
pub use accounts::{AdminAccountCreate, AdminAccountPatch, AdminAccountSecretOutput};
pub use resources::{
    DeleteQuery, ListQuery, ResourceKind, ResourceMutationOutput, ResourcePage, ResourceTarget,
    RotateCredentialInput,
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct AdminService {
    pool: PgPool,
    hash_keys: Arc<HashKeys>,
    bootstrap_hash: Option<[u8; 32]>,
    bootstrap_tenant_id: Option<Uuid>,
    session_ttl: Duration,
    credentials: CredentialService,
}

pub struct AdminServiceConfig {
    pub hash_key: Vec<u8>,
    pub hash_key_id: String,
    pub retiring_hash_keys: Vec<(String, Vec<u8>)>,
    pub bootstrap_token: Option<String>,
    pub bootstrap_tenant_id: Option<Uuid>,
    pub session_ttl: Duration,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminRole {
    Admin,
    Operator,
    Viewer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminAction {
    Logout,
    ReadResource,
    MutateResource,
    ManageRbac,
}

pub struct RbacRule {
    pub method: &'static str,
    pub route: &'static str,
    pub action: AdminAction,
    pub roles: &'static [AdminRole],
}

const ALL_ROLES: &[AdminRole] = &[AdminRole::Admin, AdminRole::Operator, AdminRole::Viewer];
const WRITERS: &[AdminRole] = &[AdminRole::Admin, AdminRole::Operator];
const ADMINS: &[AdminRole] = &[AdminRole::Admin];

pub const RBAC_RULES: &[RbacRule] = &[
    RbacRule {
        method: "DELETE",
        route: "/admin/v1/session",
        action: AdminAction::Logout,
        roles: ALL_ROLES,
    },
    RbacRule {
        method: "GET",
        route: "/admin/v1/{resource}",
        action: AdminAction::ReadResource,
        roles: ALL_ROLES,
    },
    RbacRule {
        method: "POST",
        route: "/admin/v1/{resource}",
        action: AdminAction::MutateResource,
        roles: WRITERS,
    },
    RbacRule {
        method: "GET",
        route: "/admin/v1/{resource}/{id}",
        action: AdminAction::ReadResource,
        roles: ALL_ROLES,
    },
    RbacRule {
        method: "PATCH",
        route: "/admin/v1/{resource}/{id}",
        action: AdminAction::MutateResource,
        roles: WRITERS,
    },
    RbacRule {
        method: "DELETE",
        route: "/admin/v1/{resource}/{id}",
        action: AdminAction::MutateResource,
        roles: WRITERS,
    },
    RbacRule {
        method: "POST",
        route: "/admin/v1/credentials/{id}/rotate",
        action: AdminAction::MutateResource,
        roles: WRITERS,
    },
    RbacRule {
        method: "GET",
        route: "/admin/v1/admin-accounts",
        action: AdminAction::ManageRbac,
        roles: ADMINS,
    },
    RbacRule {
        method: "POST",
        route: "/admin/v1/admin-accounts",
        action: AdminAction::ManageRbac,
        roles: ADMINS,
    },
    RbacRule {
        method: "PATCH",
        route: "/admin/v1/admin-accounts/{id}",
        action: AdminAction::ManageRbac,
        roles: ADMINS,
    },
];

pub fn authorize(method: &str, route: &str, role: AdminRole) -> Option<AdminAction> {
    RBAC_RULES
        .iter()
        .find(|rule| rule.method == method && rule.route == route)
        .filter(|rule| rule.roles.contains(&role))
        .map(|rule| rule.action)
}

impl AdminRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Operator => "operator",
            Self::Viewer => "viewer",
        }
    }

    fn parse(value: &str) -> Result<Self, AdminError> {
        match value {
            "admin" => Ok(Self::Admin),
            "operator" => Ok(Self::Operator),
            "viewer" => Ok(Self::Viewer),
            _ => Err(AdminError::Unavailable),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AdminContext {
    pub tenant_id: Uuid,
    pub account_id: Uuid,
    pub session_id: Uuid,
    pub role: AdminRole,
}

#[derive(Deserialize)]
pub struct BootstrapInput {
    pub display_name: String,
    pub credential_name: String,
}

#[derive(Serialize)]
pub struct BootstrapOutput {
    pub account_id: Uuid,
    pub credential_id: Uuid,
    pub login_token: String,
}

#[derive(Serialize)]
pub struct SessionOutput {
    pub access_token: String,
    pub expires_at: DateTime<Utc>,
    pub role: AdminRole,
}

#[derive(Debug, Error)]
pub enum AdminError {
    #[error("admin authentication failed")]
    Unauthorized,
    #[error("admin authorization failed")]
    Forbidden,
    #[error("admin resource conflict")]
    Conflict,
    #[error("admin resource was not found")]
    NotFound,
    #[error("invalid admin request")]
    InvalidRequest,
    #[error("admin control plane is unavailable")]
    Unavailable,
    #[error("could not generate admin credential")]
    Random,
    #[error("admin database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("admin credential operation failed")]
    Identity(#[from] crate::identity::IdentityError),
}

impl AdminError {
    fn kind(&self) -> &'static str {
        match self {
            Self::Unauthorized => "admin_unauthorized",
            Self::Forbidden => "admin_forbidden",
            Self::Conflict => "admin_conflict",
            Self::NotFound => "admin_not_found",
            Self::InvalidRequest => "admin_invalid_request",
            Self::Unavailable | Self::Random | Self::Database(_) | Self::Identity(_) => {
                "admin_unavailable"
            }
        }
    }

    fn cause(&self) -> Option<String> {
        match self {
            Self::Database(error) => Some(error.to_string()),
            Self::Identity(error) => Some(error.to_string()),
            _ => None,
        }
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let failure = ErrorType {
            kind: self.kind(),
            cause: self.cause(),
        };
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Conflict => StatusCode::CONFLICT,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidRequest => StatusCode::BAD_REQUEST,
            Self::Unavailable | Self::Random | Self::Database(_) | Self::Identity(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
        };
        let mut response = (
            status,
            Json(json!({"error": {"code": failure.kind, "message": self.to_string()}})),
        )
            .into_response();
        response.extensions_mut().insert(failure);
        response
    }
}

impl AdminService {
    pub fn new(pool: PgPool, credentials: CredentialService, config: AdminServiceConfig) -> Self {
        let mut values = HashMap::with_capacity(config.retiring_hash_keys.len() + 1);
        values.insert(config.hash_key_id.clone(), config.hash_key.into());
        values.extend(
            config
                .retiring_hash_keys
                .into_iter()
                .map(|(id, key)| (id, key.into())),
        );
        Self {
            pool,
            hash_keys: Arc::new(HashKeys {
                active_id: config.hash_key_id.into(),
                values,
            }),
            bootstrap_hash: config
                .bootstrap_token
                .map(|token| Sha256::digest(token.as_bytes()).into()),
            bootstrap_tenant_id: config.bootstrap_tenant_id,
            session_ttl: config.session_ttl,
            credentials,
        }
    }

    pub async fn bootstrap(
        &self,
        token: &str,
        input: BootstrapInput,
        source_ip: IpAddr,
        peer_ip: IpAddr,
        request_id: &str,
    ) -> Result<BootstrapOutput, AdminError> {
        validate_name(&input.display_name)?;
        validate_name(&input.credential_name)?;
        let tenant_id = self.bootstrap_tenant_id.ok_or(AdminError::NotFound)?;
        let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        if !self
            .bootstrap_hash
            .as_ref()
            .is_some_and(|expected| constant_time_equal(&candidate, expected))
        {
            return Err(AdminError::Unauthorized);
        }

        let account_id = Uuid::new_v4();
        let credential_id = Uuid::new_v4();
        let material = self.generate("admin", tenant_id, b"admin-login\0")?;
        let after = json!({
            "id": account_id,
            "display_name": input.display_name,
            "role": "admin",
            "status": "active",
            "version": 1
        });
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO admin_accounts (id, tenant_id, display_name, role)
            VALUES ($1, $2, $3, 'admin')
            "#,
        )
        .bind(account_id)
        .bind(tenant_id)
        .bind(&input.display_name)
        .execute(&mut *transaction)
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
        .bind(tenant_id)
        .bind(account_id)
        .bind(&input.credential_name)
        .bind(material.secret_hash.as_slice())
        .bind(&material.hash_key_id)
        .bind(&material.key_prefix)
        .bind(material.fingerprint.as_slice())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO admin_bootstrap_claims (tenant_id, admin_account_id) VALUES ($1, $2)",
        )
        .bind(tenant_id)
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .map_err(map_conflict)?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id,
                actor_id: account_id,
                role: AdminRole::Admin,
                action: "admin.bootstrap",
                target_type: "admin_account",
                target_id: Some(account_id),
                before: None,
                after: Some(after),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;

        Ok(BootstrapOutput {
            account_id,
            credential_id,
            login_token: material.plaintext,
        })
    }

    pub async fn login(
        &self,
        token: &str,
        source_ip: IpAddr,
        peer_ip: IpAddr,
        request_id: &str,
    ) -> Result<SessionOutput, AdminError> {
        let tenant_id = parse_token(token, "admin")?;
        let fingerprint: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let row = sqlx::query(
            r#"
            SELECT c.id AS credential_id, c.secret_hash, c.hash_key_id,
                   c.version AS credential_version,
                   a.id AS account_id, a.role, a.version AS account_version
            FROM admin_login_credentials c
            JOIN admin_accounts a
              ON (a.tenant_id, a.id) = (c.tenant_id, c.admin_account_id)
            WHERE c.tenant_id = $1 AND c.fingerprint = $2
              AND c.status = 'active' AND c.revoked_at IS NULL
              AND (c.expires_at IS NULL OR c.expires_at > now())
              AND a.status = 'active'
            "#,
        )
        .bind(tenant_id)
        .bind(fingerprint.as_slice())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AdminError::Unauthorized)?;
        let stored_hash = hash_from_row(&row, "secret_hash")?;
        let hash_key_id: String = row.try_get("hash_key_id")?;
        if !self.verify(token, b"admin-login\0", &hash_key_id, &stored_hash)? {
            return Err(AdminError::Unauthorized);
        }

        let account_id: Uuid = row.try_get("account_id")?;
        let credential_id: Uuid = row.try_get("credential_id")?;
        let account_version: i64 = row.try_get("account_version")?;
        let credential_version: i64 = row.try_get("credential_version")?;
        let role = AdminRole::parse(row.try_get("role")?)?;
        let session_id = Uuid::new_v4();
        let material = self.generate("session", tenant_id, b"admin-session\0")?;
        let mut transaction = self.pool.begin().await?;
        let expires_at: DateTime<Utc> = sqlx::query(
            r#"
            INSERT INTO admin_sessions (
                id, tenant_id, admin_account_id, login_credential_id,
                token_hash, hash_key_id, fingerprint, expires_at, source_ip,
                account_version, login_credential_version
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, now() + $8 * interval '1 second',
                      $9::inet, $10, $11)
            RETURNING expires_at
            "#,
        )
        .bind(session_id)
        .bind(tenant_id)
        .bind(account_id)
        .bind(credential_id)
        .bind(material.secret_hash.as_slice())
        .bind(&material.hash_key_id)
        .bind(material.fingerprint.as_slice())
        .bind(i64::try_from(self.session_ttl.as_secs()).map_err(|_| AdminError::Unavailable)?)
        .bind(source_ip.to_string())
        .bind(account_version)
        .bind(credential_version)
        .fetch_one(&mut *transaction)
        .await?
        .try_get("expires_at")?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id,
                actor_id: account_id,
                role,
                action: "admin.session.create",
                target_type: "admin_session",
                target_id: Some(session_id),
                before: None,
                after: Some(json!({"expires_at": expires_at})),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(SessionOutput {
            access_token: material.plaintext,
            expires_at,
            role,
        })
    }

    pub async fn resolve_session(&self, token: &str) -> Result<AdminContext, AdminError> {
        let tenant_id = parse_token(token, "session")?;
        let fingerprint: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let row = sqlx::query(
            r#"
            SELECT s.id AS session_id, s.token_hash, s.hash_key_id,
                   a.id AS account_id, a.role
            FROM admin_sessions s
            JOIN admin_accounts a
              ON (a.tenant_id, a.id) = (s.tenant_id, s.admin_account_id)
             AND a.version = s.account_version
            JOIN admin_login_credentials c
              ON (c.tenant_id, c.id) = (s.tenant_id, s.login_credential_id)
             AND c.version = s.login_credential_version
             AND c.admin_account_id = s.admin_account_id
            WHERE s.tenant_id = $1 AND s.fingerprint = $2
              AND s.revoked_at IS NULL AND s.expires_at > now()
              AND a.status = 'active'
              AND c.status = 'active' AND c.revoked_at IS NULL
              AND (c.expires_at IS NULL OR c.expires_at > now())
            "#,
        )
        .bind(tenant_id)
        .bind(fingerprint.as_slice())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AdminError::Unauthorized)?;
        let expected = hash_from_row(&row, "token_hash")?;
        let hash_key_id: String = row.try_get("hash_key_id")?;
        if !self.verify(token, b"admin-session\0", &hash_key_id, &expected)? {
            return Err(AdminError::Unauthorized);
        }
        Ok(AdminContext {
            tenant_id,
            account_id: row.try_get("account_id")?,
            session_id: row.try_get("session_id")?,
            role: AdminRole::parse(row.try_get("role")?)?,
        })
    }

    pub async fn logout(
        &self,
        context: &AdminContext,
        source_ip: IpAddr,
        peer_ip: IpAddr,
        request_id: &str,
    ) -> Result<(), AdminError> {
        let mut transaction = self.pool.begin().await?;
        let changed = sqlx::query(
            r#"
            UPDATE admin_sessions
            SET revoked_at = now()
            WHERE tenant_id = $1 AND id = $2 AND revoked_at IS NULL
            "#,
        )
        .bind(context.tenant_id)
        .bind(context.session_id)
        .execute(&mut *transaction)
        .await?;
        if changed.rows_affected() != 1 {
            return Err(AdminError::Unauthorized);
        }
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: "admin.session.revoke",
                target_type: "admin_session",
                target_id: Some(context.session_id),
                before: Some(json!({"revoked": false})),
                after: Some(json!({"revoked": true})),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn audit_denial(
        &self,
        context: &AdminContext,
        method: &str,
        route: &str,
        source_ip: IpAddr,
        peer_ip: IpAddr,
        request_id: &str,
    ) -> Result<(), AdminError> {
        let mut transaction = self.pool.begin().await?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: "admin.authorization.denied",
                target_type: "admin_route",
                target_id: None,
                before: None,
                after: Some(json!({"method": method, "route": route})),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    fn generate(
        &self,
        marker: &str,
        tenant_id: Uuid,
        domain: &[u8],
    ) -> Result<TokenMaterial, AdminError> {
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(|_| AdminError::Random)?;
        let encoded = URL_SAFE_NO_PAD.encode(random);
        let plaintext = format!("aigw_{marker}_{}_{}", tenant_id.simple(), encoded);
        let key_prefix = format!(
            "aigw_{marker}_{}_{}",
            &tenant_id.simple().to_string()[..8],
            &encoded[..8]
        );
        Ok(TokenMaterial {
            secret_hash: self.hash(&plaintext, domain, &self.hash_keys.active_id)?,
            hash_key_id: self.hash_keys.active_id.to_string(),
            fingerprint: Sha256::digest(plaintext.as_bytes()).into(),
            plaintext,
            key_prefix,
        })
    }

    fn hash(
        &self,
        plaintext: &str,
        domain: &[u8],
        hash_key_id: &str,
    ) -> Result<[u8; 32], AdminError> {
        let key = self
            .hash_keys
            .values
            .get(hash_key_id)
            .ok_or(AdminError::Unavailable)?;
        let mut mac = HmacSha256::new_from_slice(key).map_err(|_| AdminError::Unavailable)?;
        mac.update(domain);
        mac.update(plaintext.as_bytes());
        Ok(mac.finalize().into_bytes().into())
    }

    fn verify(
        &self,
        plaintext: &str,
        domain: &[u8],
        hash_key_id: &str,
        expected: &[u8; 32],
    ) -> Result<bool, AdminError> {
        Ok(constant_time_equal(
            &self.hash(plaintext, domain, hash_key_id)?,
            expected,
        ))
    }
}

struct TokenMaterial {
    plaintext: String,
    key_prefix: String,
    fingerprint: [u8; 32],
    secret_hash: [u8; 32],
    hash_key_id: String,
}

struct HashKeys {
    active_id: Arc<str>,
    values: HashMap<String, Arc<[u8]>>,
}

struct AuditEntry<'a> {
    tenant_id: Uuid,
    actor_id: Uuid,
    role: AdminRole,
    action: &'a str,
    target_type: &'a str,
    target_id: Option<Uuid>,
    before: Option<Value>,
    after: Option<Value>,
    source_ip: IpAddr,
    peer_ip: IpAddr,
    request_id: &'a str,
}

async fn append_audit(
    transaction: &mut Transaction<'_, Postgres>,
    entry: AuditEntry<'_>,
) -> Result<(), AdminError> {
    sqlx::query(
        r#"
        INSERT INTO audit_logs (
            id, tenant_id, actor_admin_id, actor_role, action, target_type,
            target_id, before_state, after_state, occurred_at, source_ip, peer_ip, request_id
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now(), $10::inet, $11::inet, $12)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(entry.tenant_id)
    .bind(entry.actor_id)
    .bind(entry.role.as_str())
    .bind(entry.action)
    .bind(entry.target_type)
    .bind(entry.target_id)
    .bind(entry.before)
    .bind(entry.after)
    .bind(entry.source_ip.to_string())
    .bind(entry.peer_ip.to_string())
    .bind(entry.request_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn parse_token(plaintext: &str, marker: &str) -> Result<Uuid, AdminError> {
    let mut parts = plaintext.splitn(4, '_');
    let valid = parts.next() == Some("aigw") && parts.next() == Some(marker);
    let tenant_id = parts.next().and_then(|value| Uuid::parse_str(value).ok());
    let random = parts.next().filter(|value| !value.is_empty());
    match (valid, tenant_id, random) {
        (true, Some(tenant_id), Some(_)) => Ok(tenant_id),
        _ => Err(AdminError::Unauthorized),
    }
}

fn validate_name(value: &str) -> Result<(), AdminError> {
    if value.trim().is_empty() || value.chars().count() > 200 {
        Err(AdminError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn hash_from_row(row: &sqlx::postgres::PgRow, field: &str) -> Result<[u8; 32], AdminError> {
    row.try_get::<Vec<u8>, _>(field)?
        .try_into()
        .map_err(|_| AdminError::Unavailable)
}

fn constant_time_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn map_conflict(error: sqlx::Error) -> AdminError {
    if error
        .as_database_error()
        .and_then(|database| database.constraint())
        == Some("admin_bootstrap_claims_pkey")
    {
        AdminError::Conflict
    } else {
        AdminError::Database(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{net::IpAddr, str::FromStr as _, time::Duration};

    use sha2::{Digest as _, Sha256};
    use sqlx::{PgPool, Row as _, postgres::PgPoolOptions};
    use uuid::Uuid;

    use super::{AdminError, AdminRole, AdminService, AdminServiceConfig, BootstrapInput};
    use crate::identity::CredentialService;

    const BOOTSTRAP: &str = "test-admin-bootstrap-token-at-least-32-bytes";

    fn service(pool: PgPool, tenant_id: Uuid) -> AdminService {
        service_with_keys(pool, tenant_id, "v1", Vec::new())
    }

    fn service_with_keys(
        pool: PgPool,
        tenant_id: Uuid,
        active_id: &str,
        retiring: Vec<(String, Vec<u8>)>,
    ) -> AdminService {
        let hash_key = format!("test-credential-hash-key-{active_id}-at-least-32").into_bytes();
        let credentials = CredentialService::new(
            pool.clone(),
            active_id.to_owned(),
            hash_key.clone(),
            retiring.clone(),
            "test".to_owned(),
            16,
            Duration::from_secs(60),
        );
        AdminService::new(
            pool,
            credentials,
            AdminServiceConfig {
                hash_key,
                hash_key_id: active_id.to_owned(),
                retiring_hash_keys: retiring,
                bootstrap_token: Some(BOOTSTRAP.to_owned()),
                bootstrap_tenant_id: Some(tenant_id),
                session_ttl: Duration::from_secs(900),
            },
        )
    }

    #[sqlx::test]
    async fn bootstrap_is_one_shot_and_session_revocation_is_immediate(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let service = service(pool.clone(), tenant_id);
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let first = service.bootstrap(
            BOOTSTRAP,
            BootstrapInput {
                display_name: "first owner".to_owned(),
                credential_name: "first login".to_owned(),
            },
            source,
            source,
            "bootstrap-one",
        );
        let second = service.bootstrap(
            BOOTSTRAP,
            BootstrapInput {
                display_name: "second owner".to_owned(),
                credential_name: "second login".to_owned(),
            },
            source,
            source,
            "bootstrap-two",
        );
        let (first, second) = tokio::join!(first, second);
        let issued = match (first, second) {
            (Ok(issued), Err(AdminError::Conflict)) | (Err(AdminError::Conflict), Ok(issued)) => {
                issued
            }
            _ => panic!("expected exactly one bootstrap commit"),
        };

        let counts = sqlx::query(
            r#"
            SELECT
              (SELECT count(*) FROM admin_accounts) AS accounts,
              (SELECT count(*) FROM admin_login_credentials) AS credentials,
              (SELECT count(*) FROM admin_bootstrap_claims) AS claims,
              (SELECT count(*) FROM audit_logs WHERE action = 'admin.bootstrap') AS audits
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(counts.get::<i64, _>("accounts"), 1);
        assert_eq!(counts.get::<i64, _>("credentials"), 1);
        assert_eq!(counts.get::<i64, _>("claims"), 1);
        assert_eq!(counts.get::<i64, _>("audits"), 1);

        let session = service
            .login(&issued.login_token, source, source, "login-one")
            .await
            .unwrap();
        assert_eq!(session.role, AdminRole::Admin);
        let context = service
            .resolve_session(&session.access_token)
            .await
            .unwrap();
        service
            .logout(&context, source, source, "logout-one")
            .await
            .unwrap();
        assert!(matches!(
            service.resolve_session(&session.access_token).await,
            Err(AdminError::Unauthorized)
        ));
    }

    #[sqlx::test]
    async fn account_version_change_invalidates_an_existing_session(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let service = service(pool.clone(), tenant_id);
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let issued = service
            .bootstrap(
                BOOTSTRAP,
                BootstrapInput {
                    display_name: "owner".to_owned(),
                    credential_name: "login".to_owned(),
                },
                source,
                source,
                "bootstrap",
            )
            .await
            .unwrap();
        let session = service
            .login(&issued.login_token, source, source, "login")
            .await
            .unwrap();
        service
            .resolve_session(&session.access_token)
            .await
            .unwrap();

        sqlx::query(
            "UPDATE admin_accounts SET role = 'viewer', version = version + 1 WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(issued.account_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            service.resolve_session(&session.access_token).await,
            Err(AdminError::Unauthorized)
        ));
    }

    #[sqlx::test]
    async fn retiring_hash_key_keeps_existing_login_and_session_tokens_valid(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let old_key = b"test-credential-hash-key-v1-at-least-32".to_vec();
        let old_service = service_with_keys(pool.clone(), tenant_id, "v1", Vec::new());
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let issued = old_service
            .bootstrap(
                BOOTSTRAP,
                BootstrapInput {
                    display_name: "owner".to_owned(),
                    credential_name: "login".to_owned(),
                },
                source,
                source,
                "bootstrap",
            )
            .await
            .unwrap();
        let old_session = old_service
            .login(&issued.login_token, source, source, "old-login")
            .await
            .unwrap();

        let rotated = service_with_keys(
            pool.clone(),
            tenant_id,
            "v2",
            vec![("v1".to_owned(), old_key)],
        );
        rotated
            .resolve_session(&old_session.access_token)
            .await
            .expect("retiring key must verify a pre-rotation session");
        let new_session = rotated
            .login(&issued.login_token, source, source, "rotated-login")
            .await
            .expect("retiring key must verify a pre-rotation login token");
        rotated
            .resolve_session(&new_session.access_token)
            .await
            .unwrap();

        let fingerprint: [u8; 32] = Sha256::digest(new_session.access_token.as_bytes()).into();
        let new_hash_key_id: String =
            sqlx::query_scalar("SELECT hash_key_id FROM admin_sessions WHERE fingerprint = $1")
                .bind(fingerprint.as_slice())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(new_hash_key_id, "v2");
    }

    #[sqlx::test]
    async fn a_saturated_admin_pool_does_not_consume_the_data_plane_pool(pool: PgPool) {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy_with(pool.connect_options().as_ref().clone());
        let service = service(admin_pool, Uuid::new_v4());
        let held_admin_connection = service.pool.acquire().await.unwrap();
        let blocked_admin =
            tokio::time::timeout(Duration::from_millis(50), service.pool.acquire()).await;
        assert!(blocked_admin.is_err(), "admin pool was not saturated");

        let data_plane_value: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(data_plane_value, 1);
        drop(held_admin_connection);
    }
}
