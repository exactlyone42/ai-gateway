use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, Pool, Postgres};
use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;
const MAX_ROTATION_OVERLAP: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("invalid credential")]
    InvalidCredential,
    #[error("credential state does not allow this operation")]
    InvalidTransition,
    #[error("rotation overlap must be between one second and fifteen minutes")]
    InvalidOverlap,
    #[error("credential hashing is unavailable")]
    InvalidHashKey,
    #[error("credential hash key version is unavailable")]
    HashKeyUnavailable,
    #[error("secure random generation failed")]
    Random(#[source] getrandom::Error),
    #[error("credential cache is unavailable")]
    CacheUnavailable,
    #[error("identity store is unavailable")]
    Database(#[source] sqlx::Error),
}

impl From<sqlx::Error> for IdentityError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

/// The logical caller resolved once at the authentication boundary.
///
/// The dimension tables do not point at each other. A credential selects the
/// dimensions that make up this request's principal without fixing a hierarchy
/// in Rust or in the database.
#[derive(Clone, PartialEq, Eq, FromRow)]
pub struct PrincipalContext {
    pub tenant_id: Uuid,
    pub credential_id: Uuid,
    pub credential_name: String,
    pub user_id: Option<Uuid>,
    pub user_name: Option<String>,
    pub device_id: Option<Uuid>,
    pub device_name: Option<String>,
    pub client_id: Option<Uuid>,
    pub client_name: Option<String>,
    pub agent_id: Option<Uuid>,
    pub agent_name: Option<String>,
}

impl PrincipalContext {
    pub fn bootstrap() -> Self {
        Self {
            tenant_id: Uuid::nil(),
            credential_id: Uuid::nil(),
            credential_name: "development bootstrap".to_owned(),
            user_id: None,
            user_name: None,
            device_id: None,
            device_name: None,
            client_id: None,
            client_name: None,
            agent_id: None,
            agent_name: None,
        }
    }

    /// Logs use an opaque identifier. Display names remain available to the
    /// control plane but are not telemetry fields because they can contain PII.
    pub fn log_id(&self) -> String {
        self.credential_id.simple().to_string()
    }
}

#[derive(Clone, Debug, FromRow, PartialEq, Eq)]
pub struct PrincipalIds {
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub client_id: Uuid,
    pub agent_id: Uuid,
}

pub struct NewPrincipal<'a> {
    pub tenant_id: Uuid,
    pub user_name: &'a str,
    pub device_name: &'a str,
    pub client_name: &'a str,
    pub agent_name: &'a str,
}

pub struct NewCredential<'a> {
    pub tenant_id: Uuid,
    pub display_name: &'a str,
    pub principal: PrincipalIds,
    pub expires_at: Option<DateTime<Utc>>,
}

pub struct IssuedCredential {
    pub id: Uuid,
    pub plaintext: String,
    pub key_prefix: String,
    pub fingerprint: String,
}

#[derive(Clone)]
pub struct CredentialService {
    pool: Pool<Postgres>,
    hash_keys: Arc<HashKeys>,
    environment: Arc<str>,
    cache_ttl: Duration,
    cache: Arc<Mutex<CredentialCache>>,
    lifecycle_gate: Arc<RwLock<()>>,
}

impl CredentialService {
    pub fn new(
        pool: Pool<Postgres>,
        active_hash_key_id: String,
        active_hash_key: Vec<u8>,
        retiring_hash_keys: Vec<(String, Vec<u8>)>,
        environment: String,
        cache_capacity: usize,
        cache_ttl: Duration,
    ) -> Self {
        let mut values = HashMap::with_capacity(retiring_hash_keys.len() + 1);
        values.insert(active_hash_key_id.clone(), active_hash_key.into());
        values.extend(
            retiring_hash_keys
                .into_iter()
                .map(|(id, key)| (id, key.into())),
        );
        Self {
            pool,
            hash_keys: Arc::new(HashKeys {
                active_id: active_hash_key_id.into(),
                values,
            }),
            environment: environment.into(),
            cache_ttl,
            cache: Arc::new(Mutex::new(CredentialCache::new(cache_capacity))),
            lifecycle_gate: Arc::new(RwLock::new(())),
        }
    }

    pub async fn create_principal(
        &self,
        input: NewPrincipal<'_>,
    ) -> Result<PrincipalIds, IdentityError> {
        let ids = PrincipalIds {
            user_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            client_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
        };
        sqlx::query_as::<_, PrincipalIds>(
            r#"
            WITH new_user AS (
                INSERT INTO users (id, tenant_id, display_name)
                VALUES ($1, $5, $6)
                RETURNING id
            ), new_device AS (
                INSERT INTO devices (id, tenant_id, display_name)
                VALUES ($2, $5, $7)
                RETURNING id
            ), new_client AS (
                INSERT INTO clients (id, tenant_id, display_name)
                VALUES ($3, $5, $8)
                RETURNING id
            ), new_agent AS (
                INSERT INTO agents (id, tenant_id, display_name)
                VALUES ($4, $5, $9)
                RETURNING id
            )
            SELECT new_user.id AS user_id,
                   new_device.id AS device_id,
                   new_client.id AS client_id,
                   new_agent.id AS agent_id
            FROM new_user, new_device, new_client, new_agent
            "#,
        )
        .bind(ids.user_id)
        .bind(ids.device_id)
        .bind(ids.client_id)
        .bind(ids.agent_id)
        .bind(input.tenant_id)
        .bind(input.user_name)
        .bind(input.device_name)
        .bind(input.client_name)
        .bind(input.agent_name)
        .fetch_one(&self.pool)
        .await
        .map_err(IdentityError::from)
    }

    pub async fn create(
        &self,
        input: NewCredential<'_>,
    ) -> Result<IssuedCredential, IdentityError> {
        let material = self.generate(input.tenant_id)?;
        let id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO credentials (
                id, tenant_id, "type", display_name, secret_hash, hash_key_id, key_prefix,
                fingerprint, user_id, device_id, client_id, agent_id, expires_at
            ) VALUES (
                $1, $2, 'client_api_key', $3, $4, $5, $6,
                $7, $8, $9, $10, $11, $12
            )
            "#,
        )
        .bind(id)
        .bind(input.tenant_id)
        .bind(input.display_name)
        .bind(material.secret_hash.as_slice())
        .bind(&material.hash_key_id)
        .bind(&material.key_prefix)
        .bind(material.fingerprint.as_slice())
        .bind(input.principal.user_id)
        .bind(input.principal.device_id)
        .bind(input.principal.client_id)
        .bind(input.principal.agent_id)
        .bind(input.expires_at)
        .execute(&self.pool)
        .await?;

        Ok(material.issued(id))
    }

    #[tracing::instrument(name = "auth.resolve", skip_all)]
    pub async fn resolve(&self, plaintext: &str) -> Result<PrincipalContext, IdentityError> {
        let _resolution = self.lifecycle_gate.read().await;
        let tenant_id = parse_tenant(plaintext, &self.environment)?;
        let fingerprint = fingerprint(plaintext);
        let key = CacheKey {
            tenant_id,
            fingerprint,
        };

        if let Some(cached) = self.cache_get(&key)? {
            return if self.verify_secret(plaintext, &cached.hash_key_id, &cached.secret_hash)? {
                Ok(cached.principal)
            } else {
                Err(IdentityError::InvalidCredential)
            };
        }

        let row = sqlx::query_as::<_, CredentialRow>(
            r#"
            WITH expired AS (
                UPDATE credentials
                SET status = 'expired'
                WHERE tenant_id = $1
                  AND fingerprint = $2
                  AND status IN ('active', 'disabled')
                  AND expires_at <= now()
            ), touched AS (
                UPDATE credentials
                SET last_used_at = now()
                WHERE tenant_id = $1
                  AND fingerprint = $2
                  AND status = 'active'
                  AND revoked_at IS NULL
                  AND (expires_at IS NULL OR expires_at > now())
                RETURNING *
            )
            SELECT c.secret_hash, c.hash_key_id, c.expires_at,
                   c.tenant_id, c.id AS credential_id,
                   c.display_name AS credential_name,
                   c.user_id, u.display_name AS user_name,
                   c.device_id, d.display_name AS device_name,
                   c.client_id, cl.display_name AS client_name,
                   c.agent_id, a.display_name AS agent_name
            FROM touched c
            LEFT JOIN users u
              ON u.tenant_id = c.tenant_id AND u.id = c.user_id
            LEFT JOIN devices d
              ON d.tenant_id = c.tenant_id AND d.id = c.device_id
            LEFT JOIN clients cl
              ON cl.tenant_id = c.tenant_id AND cl.id = c.client_id
            LEFT JOIN agents a
              ON a.tenant_id = c.tenant_id AND a.id = c.agent_id
            "#,
        )
        .bind(tenant_id)
        .bind(fingerprint.as_slice())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(IdentityError::InvalidCredential)?;

        let stored_hash: [u8; 32] = row
            .secret_hash
            .as_slice()
            .try_into()
            .map_err(|_| IdentityError::InvalidCredential)?;
        if !self.verify_secret(plaintext, &row.hash_key_id, &stored_hash)? {
            return Err(IdentityError::InvalidCredential);
        }

        let principal = row.principal();
        let valid_for = cache_duration(self.cache_ttl, row.expires_at);
        self.cache_insert(
            key,
            CachedCredential {
                credential_id: principal.credential_id,
                principal: principal.clone(),
                secret_hash: stored_hash,
                hash_key_id: row.hash_key_id,
                valid_until: Instant::now()
                    .checked_add(valid_for)
                    .ok_or(IdentityError::CacheUnavailable)?,
            },
        )?;
        Ok(principal)
    }

    pub async fn revoke(&self, tenant_id: Uuid, id: Uuid) -> Result<(), IdentityError> {
        let _lifecycle = self.lifecycle_gate.write().await;
        self.cache_lock()?.invalidate(id);
        self.change_status(
            tenant_id,
            id,
            r#"
            UPDATE credentials
            SET status = 'revoked', revoked_at = now()
            WHERE tenant_id = $1 AND id = $2
              AND status IN ('active', 'disabled')
            RETURNING id
            "#,
        )
        .await?;
        Ok(())
    }

    pub async fn disable(&self, tenant_id: Uuid, id: Uuid) -> Result<(), IdentityError> {
        let _lifecycle = self.lifecycle_gate.write().await;
        self.cache_lock()?.invalidate(id);
        self.change_status(
            tenant_id,
            id,
            r#"
            UPDATE credentials
            SET status = 'disabled'
            WHERE tenant_id = $1 AND id = $2 AND status = 'active'
              AND (expires_at IS NULL OR expires_at > now())
            RETURNING id
            "#,
        )
        .await?;
        Ok(())
    }

    pub async fn enable(&self, tenant_id: Uuid, id: Uuid) -> Result<(), IdentityError> {
        let _lifecycle = self.lifecycle_gate.write().await;
        self.cache_lock()?.invalidate(id);
        self.change_status(
            tenant_id,
            id,
            r#"
            UPDATE credentials
            SET status = 'active'
            WHERE tenant_id = $1 AND id = $2 AND status = 'disabled'
              AND revoked_at IS NULL
              AND (expires_at IS NULL OR expires_at > now())
            RETURNING id
            "#,
        )
        .await?;
        Ok(())
    }

    pub async fn expire_due(&self, tenant_id: Uuid) -> Result<u64, IdentityError> {
        let _lifecycle = self.lifecycle_gate.write().await;
        self.cache_lock()?.clear_tenant(tenant_id);
        let result = sqlx::query(
            r#"
            UPDATE credentials
            SET status = 'expired'
            WHERE tenant_id = $1
              AND status IN ('active', 'disabled')
              AND expires_at <= now()
            "#,
        )
        .bind(tenant_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn rotate(
        &self,
        tenant_id: Uuid,
        id: Uuid,
        overlap: Duration,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<IssuedCredential, IdentityError> {
        if overlap.is_zero() || overlap > MAX_ROTATION_OVERLAP {
            return Err(IdentityError::InvalidOverlap);
        }
        let _lifecycle = self.lifecycle_gate.write().await;
        self.cache_lock()?.invalidate(id);
        let material = self.generate(tenant_id)?;
        let new_id = Uuid::new_v4();
        let overlap_seconds =
            i64::try_from(overlap.as_secs()).map_err(|_| IdentityError::InvalidOverlap)?;

        let inserted = sqlx::query_scalar::<_, Uuid>(
            r#"
            WITH old AS (
                SELECT *
                FROM credentials
                WHERE tenant_id = $1 AND id = $2 AND status = 'active'
                  AND revoked_at IS NULL
                  AND (expires_at IS NULL OR expires_at > now())
                FOR UPDATE
            ), shortened AS (
                UPDATE credentials current
                SET expires_at = LEAST(
                    COALESCE(current.expires_at, now() + $3 * interval '1 second'),
                    now() + $3 * interval '1 second'
                )
                FROM old
                WHERE current.tenant_id = old.tenant_id AND current.id = old.id
                RETURNING old."type", old.display_name, old.user_id,
                          old.device_id, old.client_id, old.agent_id,
                          old.expires_at AS inherited_expires_at
            ), inserted AS (
                INSERT INTO credentials (
                    id, tenant_id, "type", display_name, secret_hash, hash_key_id, key_prefix,
                    fingerprint, user_id, device_id, client_id, agent_id,
                    expires_at, rotated_from_id
                )
                SELECT $4, $1, "type", display_name, $5, $6, $7,
                       $8, user_id, device_id, client_id, agent_id,
                       COALESCE($9, inherited_expires_at), $2
                FROM shortened
                RETURNING id
            )
            SELECT id FROM inserted
            "#,
        )
        .bind(tenant_id)
        .bind(id)
        .bind(overlap_seconds)
        .bind(new_id)
        .bind(material.secret_hash.as_slice())
        .bind(&material.hash_key_id)
        .bind(&material.key_prefix)
        .bind(material.fingerprint.as_slice())
        .bind(expires_at)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_rotation_error)?
        .ok_or(IdentityError::InvalidTransition)?;

        Ok(material.issued(inserted))
    }

    async fn change_status(
        &self,
        tenant_id: Uuid,
        id: Uuid,
        statement: &str,
    ) -> Result<Uuid, IdentityError> {
        sqlx::query_scalar::<_, Uuid>(statement)
            .bind(tenant_id)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(IdentityError::InvalidTransition)
    }

    fn generate(&self, tenant_id: Uuid) -> Result<KeyMaterial, IdentityError> {
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(IdentityError::Random)?;
        let encoded = URL_SAFE_NO_PAD.encode(random);
        let plaintext = format!(
            "aigw_{}_{}_{}",
            self.environment,
            tenant_id.simple(),
            encoded
        );
        let key_prefix = format!(
            "aigw_{}_{}_{}",
            self.environment,
            &tenant_id.simple().to_string()[..8],
            &encoded[..8]
        );
        Ok(KeyMaterial {
            secret_hash: self.secret_hash(&plaintext, &self.hash_keys.active_id)?,
            hash_key_id: self.hash_keys.active_id.to_string(),
            fingerprint: fingerprint(&plaintext),
            plaintext,
            key_prefix,
        })
    }

    fn secret_hash(&self, plaintext: &str, hash_key_id: &str) -> Result<[u8; 32], IdentityError> {
        let hash_key = self
            .hash_keys
            .values
            .get(hash_key_id)
            .ok_or(IdentityError::HashKeyUnavailable)?;
        let mut mac = <HmacSha256 as Mac>::new_from_slice(hash_key)
            .map_err(|_| IdentityError::InvalidHashKey)?;
        mac.update(plaintext.as_bytes());
        Ok(mac.finalize().into_bytes().into())
    }

    fn verify_secret(
        &self,
        plaintext: &str,
        hash_key_id: &str,
        expected: &[u8; 32],
    ) -> Result<bool, IdentityError> {
        let hash_key = self
            .hash_keys
            .values
            .get(hash_key_id)
            .ok_or(IdentityError::HashKeyUnavailable)?;
        let mut mac = <HmacSha256 as Mac>::new_from_slice(hash_key)
            .map_err(|_| IdentityError::InvalidHashKey)?;
        mac.update(plaintext.as_bytes());
        Ok(mac.verify_slice(expected).is_ok())
    }

    fn cache_get(&self, key: &CacheKey) -> Result<Option<CachedCredential>, IdentityError> {
        Ok(self.cache_lock()?.get(key))
    }

    fn cache_insert(&self, key: CacheKey, value: CachedCredential) -> Result<(), IdentityError> {
        self.cache_lock()?.insert(key, value);
        Ok(())
    }

    fn cache_lock(&self) -> Result<std::sync::MutexGuard<'_, CredentialCache>, IdentityError> {
        self.cache
            .lock()
            .map_err(|_| IdentityError::CacheUnavailable)
    }
}

struct HashKeys {
    active_id: Arc<str>,
    values: HashMap<String, Arc<[u8]>>,
}

fn map_rotation_error(error: sqlx::Error) -> IdentityError {
    if error
        .as_database_error()
        .and_then(|database| database.constraint())
        == Some("credentials_one_successor")
    {
        IdentityError::InvalidTransition
    } else {
        IdentityError::Database(error)
    }
}

struct KeyMaterial {
    plaintext: String,
    key_prefix: String,
    fingerprint: [u8; 32],
    secret_hash: [u8; 32],
    hash_key_id: String,
}

impl KeyMaterial {
    fn issued(self, id: Uuid) -> IssuedCredential {
        IssuedCredential {
            id,
            plaintext: self.plaintext,
            key_prefix: self.key_prefix,
            fingerprint: URL_SAFE_NO_PAD.encode(self.fingerprint),
        }
    }
}

#[derive(FromRow)]
struct CredentialRow {
    secret_hash: Vec<u8>,
    hash_key_id: String,
    expires_at: Option<DateTime<Utc>>,
    tenant_id: Uuid,
    credential_id: Uuid,
    credential_name: String,
    user_id: Option<Uuid>,
    user_name: Option<String>,
    device_id: Option<Uuid>,
    device_name: Option<String>,
    client_id: Option<Uuid>,
    client_name: Option<String>,
    agent_id: Option<Uuid>,
    agent_name: Option<String>,
}

impl CredentialRow {
    fn principal(&self) -> PrincipalContext {
        PrincipalContext {
            tenant_id: self.tenant_id,
            credential_id: self.credential_id,
            credential_name: self.credential_name.clone(),
            user_id: self.user_id,
            user_name: self.user_name.clone(),
            device_id: self.device_id,
            device_name: self.device_name.clone(),
            client_id: self.client_id,
            client_name: self.client_name.clone(),
            agent_id: self.agent_id,
            agent_name: self.agent_name.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct CacheKey {
    tenant_id: Uuid,
    fingerprint: [u8; 32],
}

#[derive(Clone)]
struct CachedCredential {
    credential_id: Uuid,
    principal: PrincipalContext,
    secret_hash: [u8; 32],
    hash_key_id: String,
    valid_until: Instant,
}

struct CredentialCache {
    capacity: usize,
    entries: HashMap<CacheKey, CachedCredential>,
    order: VecDeque<CacheKey>,
}

impl CredentialCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
        }
    }

    fn get(&mut self, key: &CacheKey) -> Option<CachedCredential> {
        let entry = self.entries.get(key)?.clone();
        if entry.valid_until <= Instant::now() {
            self.entries.remove(key);
            self.order.retain(|candidate| candidate != key);
            return None;
        }
        self.order.retain(|candidate| candidate != key);
        self.order.push_back(*key);
        Some(entry)
    }

    fn insert(&mut self, key: CacheKey, value: CachedCredential) {
        if self.capacity == 0 {
            return;
        }
        self.entries.insert(key, value);
        self.order.retain(|candidate| candidate != &key);
        self.order.push_back(key);
        while self.entries.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.entries.remove(&evicted);
            } else {
                self.entries.clear();
                break;
            }
        }
    }

    fn invalidate(&mut self, credential_id: Uuid) {
        self.entries
            .retain(|_, entry| entry.credential_id != credential_id);
        self.order.retain(|key| self.entries.contains_key(key));
    }

    fn clear_tenant(&mut self, tenant_id: Uuid) {
        self.entries.retain(|key, _| key.tenant_id != tenant_id);
        self.order.retain(|key| self.entries.contains_key(key));
    }
}

fn parse_tenant(plaintext: &str, environment: &str) -> Result<Uuid, IdentityError> {
    let mut parts = plaintext.splitn(4, '_');
    let marker = parts.next();
    let key_environment = parts.next();
    let tenant = parts.next();
    let encoded = parts.next();
    if marker != Some("aigw") || key_environment != Some(environment) {
        return Err(IdentityError::InvalidCredential);
    }
    let tenant = tenant
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(IdentityError::InvalidCredential)?;
    let decoded = encoded
        .and_then(|value| URL_SAFE_NO_PAD.decode(value).ok())
        .ok_or(IdentityError::InvalidCredential)?;
    if decoded.len() != 32 {
        return Err(IdentityError::InvalidCredential);
    }
    Ok(tenant)
}

fn fingerprint(plaintext: &str) -> [u8; 32] {
    Sha256::digest(plaintext.as_bytes()).into()
}

fn cache_duration(ttl: Duration, expires_at: Option<DateTime<Utc>>) -> Duration {
    let Some(expires_at) = expires_at else {
        return ttl;
    };
    expires_at
        .signed_duration_since(Utc::now())
        .to_std()
        .unwrap_or_default()
        .min(ttl)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt as _;

    use super::*;
    use crate::{config::Config, routes, state::AppState};

    fn principal(id: Uuid) -> PrincipalContext {
        PrincipalContext {
            tenant_id: Uuid::new_v4(),
            credential_id: id,
            credential_name: "test key".to_owned(),
            user_id: None,
            user_name: None,
            device_id: None,
            device_name: None,
            client_id: Some(Uuid::new_v4()),
            client_name: Some("test client".to_owned()),
            agent_id: None,
            agent_name: None,
        }
    }

    #[test]
    fn bounded_cache_evicts_the_oldest_credential() {
        let mut cache = CredentialCache::new(1);
        let first = CacheKey {
            tenant_id: Uuid::new_v4(),
            fingerprint: [1; 32],
        };
        let second = CacheKey {
            tenant_id: Uuid::new_v4(),
            fingerprint: [2; 32],
        };
        cache.insert(
            first,
            CachedCredential {
                credential_id: Uuid::new_v4(),
                principal: principal(Uuid::new_v4()),
                secret_hash: [1; 32],
                hash_key_id: "v1".to_owned(),
                valid_until: Instant::now() + Duration::from_secs(60),
            },
        );
        cache.insert(
            second,
            CachedCredential {
                credential_id: Uuid::new_v4(),
                principal: principal(Uuid::new_v4()),
                secret_hash: [2; 32],
                hash_key_id: "v1".to_owned(),
                valid_until: Instant::now() + Duration::from_secs(60),
            },
        );

        assert!(cache.get(&first).is_none());
        assert!(cache.get(&second).is_some());
        assert_eq!(cache.entries.len(), 1);
    }

    #[tokio::test]
    async fn hmac_verification_rejects_a_different_key() {
        let pool = crate::db::pool("postgres://gateway@127.0.0.1:1/gateway", 1).unwrap();
        let service = CredentialService::new(
            pool,
            "v1".to_owned(),
            b"test-credential-hash-key-at-least-32".to_vec(),
            Vec::new(),
            "test".to_owned(),
            1,
            Duration::from_secs(60),
        );
        let expected = service.secret_hash("first", "v1").unwrap();
        assert!(service.verify_secret("first", "v1", &expected).unwrap());
        assert!(!service.verify_secret("second", "v1", &expected).unwrap());
    }

    #[sqlx::test]
    async fn lifecycle_is_tenant_safe_and_revocation_beats_the_cache(pool: Pool<Postgres>) {
        let config = Config {
            listen_address: "127.0.0.1:0".parse().unwrap(),
            openai_base_url: "http://127.0.0.1:1".to_owned(),
            openai_api_key: "upstream-key".to_owned(),
            openai_provider: "openai-test".to_owned(),
            openai_request_max_bytes: 32 * 1024 * 1024,
            anthropic_base_url: "http://127.0.0.1:1".to_owned(),
            anthropic_api_key: "provider-key".to_owned(),
            anthropic_provider: "anthropic-test".to_owned(),
            anthropic_request_max_bytes: 32 * 1024 * 1024,
            stream_event_max_bytes: 32 * 1024 * 1024,
            bootstrap_client_key: Some("bootstrap-key".to_owned()),
            allowed_models: vec!["company-default".to_owned()],
            otlp_endpoint: None,
            database_url: "managed-by-sqlx-test".to_owned(),
            database_max_connections: 1,
            credential_hash_key: b"test-credential-hash-key-at-least-32".to_vec(),
            credential_hash_key_id: "v1".to_owned(),
            credential_retiring_hash_keys: Vec::new(),
            credential_environment: "test".to_owned(),
            credential_cache_capacity: 2,
            credential_cache_ttl_seconds: 300,
            ledger_spool_dir: std::env::temp_dir().join(format!(
                "agent-pool-gateway-identity-test-{}",
                Uuid::new_v4()
            )),
            ledger_spool_max_bytes: 1024 * 1024,
            ledger_spool_entry_max_bytes: 32 * 1024,
        };
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
        let tenant_id = Uuid::new_v4();
        let principal = state
            .credentials
            .create_principal(NewPrincipal {
                tenant_id,
                user_name: "user one",
                device_name: "device one",
                client_name: "client one",
                agent_name: "agent one",
            })
            .await
            .unwrap();
        let issued = state
            .credentials
            .create(NewCredential {
                tenant_id,
                display_name: "primary key",
                principal: principal.clone(),
                expires_at: None,
            })
            .await
            .unwrap();

        assert!(!database_contains(&pool, &issued.plaintext).await);
        assert_eq!(
            state
                .credentials
                .resolve(&issued.plaintext)
                .await
                .unwrap()
                .user_id,
            Some(principal.user_id)
        );
        let was_used = sqlx::query_scalar::<_, bool>(
            "SELECT last_used_at IS NOT NULL FROM credentials WHERE id = $1",
        )
        .bind(issued.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            was_used,
            "successful resolution did not update last_used_at"
        );

        state
            .credentials
            .disable(tenant_id, issued.id)
            .await
            .unwrap();
        assert!(matches!(
            state.credentials.resolve(&issued.plaintext).await,
            Err(IdentityError::InvalidCredential)
        ));
        state
            .credentials
            .enable(tenant_id, issued.id)
            .await
            .unwrap();

        let rotated = state
            .credentials
            .rotate(tenant_id, issued.id, Duration::from_secs(30), None)
            .await
            .unwrap();
        assert!(state.credentials.resolve(&issued.plaintext).await.is_ok());
        assert!(state.credentials.resolve(&rotated.plaintext).await.is_ok());

        let replacement_hash_key = b"replacement-credential-hash-key-32-plus".to_vec();
        let overlapping_hash_keys = CredentialService::new(
            pool.clone(),
            "v2".to_owned(),
            replacement_hash_key,
            vec![(
                "v1".to_owned(),
                b"test-credential-hash-key-at-least-32".to_vec(),
            )],
            "test".to_owned(),
            2,
            Duration::from_secs(300),
        );
        assert!(
            overlapping_hash_keys
                .resolve(&rotated.plaintext)
                .await
                .is_ok(),
            "a retiring server-side hash key stopped resolving existing credentials"
        );
        let v2_issued = overlapping_hash_keys
            .create(NewCredential {
                tenant_id,
                display_name: "v2 hash key",
                principal: principal.clone(),
                expires_at: None,
            })
            .await
            .unwrap();
        let hash_key_ids = sqlx::query_scalar::<_, String>(
            "SELECT hash_key_id FROM credentials WHERE id = ANY($1) ORDER BY hash_key_id",
        )
        .bind(vec![rotated.id, v2_issued.id])
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(hash_key_ids, vec!["v1", "v2"]);

        let before_revoke = authenticated_request(&state, &issued.plaintext).await;
        assert_eq!(before_revoke, StatusCode::BAD_GATEWAY);
        let held_resolution = state.credentials.lifecycle_gate.clone().read_owned().await;
        let revoking_service = state.credentials.clone();
        let revoke =
            tokio::spawn(async move { revoking_service.revoke(tenant_id, issued.id).await });
        tokio::task::yield_now().await;
        assert!(
            !revoke.is_finished(),
            "revoke bypassed an in-flight resolution and could miss its cache fill"
        );
        drop(held_resolution);
        revoke.await.unwrap().unwrap();
        let after_revoke = authenticated_request(&state, &issued.plaintext).await;
        assert_eq!(after_revoke, StatusCode::UNAUTHORIZED);

        let concurrent_rotation = state
            .credentials
            .create(NewCredential {
                tenant_id,
                display_name: "concurrent rotation",
                principal: principal.clone(),
                expires_at: None,
            })
            .await
            .unwrap();
        let (first_rotation, second_rotation) = tokio::join!(
            state.credentials.rotate(
                tenant_id,
                concurrent_rotation.id,
                Duration::from_secs(30),
                None
            ),
            state.credentials.rotate(
                tenant_id,
                concurrent_rotation.id,
                Duration::from_secs(30),
                None
            )
        );
        assert_eq!(
            usize::from(first_rotation.is_ok()) + usize::from(second_rotation.is_ok()),
            1,
            "one source credential produced more than one active successor"
        );
        let successor_count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM credentials WHERE tenant_id = $1 AND rotated_from_id = $2",
        )
        .bind(tenant_id)
        .bind(concurrent_rotation.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(successor_count, 1);

        let expiring = state
            .credentials
            .create(NewCredential {
                tenant_id,
                display_name: "expiring key",
                principal: principal.clone(),
                expires_at: None,
            })
            .await
            .unwrap();
        sqlx::query(
            "UPDATE credentials SET created_at = now() - interval '2 seconds', \
             expires_at = now() - interval '1 second' WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(expiring.id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            state.credentials.resolve(&expiring.plaintext).await,
            Err(IdentityError::InvalidCredential)
        ));
        let expired_status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM credentials WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(expiring.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(expired_status, "expired");

        let other_tenant = Uuid::new_v4();
        let cross_tenant = state
            .credentials
            .create(NewCredential {
                tenant_id: other_tenant,
                display_name: "must fail",
                principal,
                expires_at: None,
            })
            .await;
        assert!(matches!(cross_tenant, Err(IdentityError::Database(_))));

        sqlx::query("UPDATE credentials SET display_name = $1 WHERE id = $2")
            .bind(&rotated.plaintext)
            .bind(rotated.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            database_contains(&pool, &rotated.plaintext).await,
            "the plaintext scanner did not detect an injected leak"
        );

        state.credentials.resolve(&rotated.plaintext).await.unwrap();
        pool.close().await;
        assert!(
            state.credentials.resolve(&rotated.plaintext).await.is_ok(),
            "a cached credential stopped working when the database closed"
        );
    }

    #[sqlx::test]
    async fn a_locked_credential_fails_closed_within_the_database_deadline(pool: Pool<Postgres>) {
        let bounded_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(crate::db::gateway_options(
                pool.connect_options().as_ref().clone(),
            ))
            .await
            .unwrap();
        let config = Config {
            listen_address: "127.0.0.1:0".parse().unwrap(),
            openai_base_url: "http://127.0.0.1:1".to_owned(),
            openai_api_key: "upstream-key".to_owned(),
            openai_provider: "openai-test".to_owned(),
            openai_request_max_bytes: 32 * 1024 * 1024,
            anthropic_base_url: "http://127.0.0.1:1".to_owned(),
            anthropic_api_key: "provider-key".to_owned(),
            anthropic_provider: "anthropic-test".to_owned(),
            anthropic_request_max_bytes: 32 * 1024 * 1024,
            stream_event_max_bytes: 32 * 1024 * 1024,
            bootstrap_client_key: None,
            allowed_models: vec!["company-default".to_owned()],
            otlp_endpoint: None,
            database_url: "managed-by-sqlx-test".to_owned(),
            database_max_connections: 2,
            credential_hash_key: b"test-credential-hash-key-at-least-32".to_vec(),
            credential_hash_key_id: "v1".to_owned(),
            credential_retiring_hash_keys: Vec::new(),
            credential_environment: "test".to_owned(),
            credential_cache_capacity: 2,
            credential_cache_ttl_seconds: 300,
            ledger_spool_dir: std::env::temp_dir().join(format!(
                "agent-pool-gateway-identity-test-{}",
                Uuid::new_v4()
            )),
            ledger_spool_max_bytes: 1024 * 1024,
            ledger_spool_entry_max_bytes: 32 * 1024,
        };
        let state = Arc::new(AppState::new(config, bounded_pool).unwrap());
        let tenant_id = Uuid::new_v4();
        let principal = state
            .credentials
            .create_principal(NewPrincipal {
                tenant_id,
                user_name: "locked user",
                device_name: "locked device",
                client_name: "locked client",
                agent_name: "locked agent",
            })
            .await
            .unwrap();
        let issued = state
            .credentials
            .create(NewCredential {
                tenant_id,
                display_name: "locked key",
                principal,
                expires_at: None,
            })
            .await
            .unwrap();

        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM credentials WHERE id = $1 FOR UPDATE")
            .bind(issued.id)
            .execute(&mut *blocker)
            .await
            .unwrap();

        let started = Instant::now();
        let status = authenticated_request(&state, &issued.plaintext).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a locked credential stalled authentication beyond its lock timeout"
        );

        blocker.rollback().await.unwrap();
        assert_eq!(
            authenticated_request(&state, &issued.plaintext).await,
            StatusCode::BAD_GATEWAY,
            "authentication did not recover after the row lock was released"
        );
    }

    #[tokio::test]
    async fn production_bootstrap_is_rejected_and_database_miss_fails_closed() {
        let config = Config {
            listen_address: "127.0.0.1:0".parse().unwrap(),
            openai_base_url: "http://127.0.0.1:1".to_owned(),
            openai_api_key: "upstream-key".to_owned(),
            openai_provider: "openai-test".to_owned(),
            openai_request_max_bytes: 32 * 1024 * 1024,
            anthropic_base_url: "http://127.0.0.1:1".to_owned(),
            anthropic_api_key: "provider-key".to_owned(),
            anthropic_provider: "anthropic-test".to_owned(),
            anthropic_request_max_bytes: 32 * 1024 * 1024,
            stream_event_max_bytes: 32 * 1024 * 1024,
            bootstrap_client_key: None,
            allowed_models: vec!["company-default".to_owned()],
            otlp_endpoint: None,
            database_url: "postgres://gateway@127.0.0.1:1/gateway".to_owned(),
            database_max_connections: 1,
            credential_hash_key: b"test-credential-hash-key-at-least-32".to_vec(),
            credential_hash_key_id: "v1".to_owned(),
            credential_retiring_hash_keys: Vec::new(),
            credential_environment: "test".to_owned(),
            credential_cache_capacity: 2,
            credential_cache_ttl_seconds: 300,
            ledger_spool_dir: std::env::temp_dir().join(format!(
                "agent-pool-gateway-identity-test-{}",
                Uuid::new_v4()
            )),
            ledger_spool_max_bytes: 1024 * 1024,
            ledger_spool_entry_max_bytes: 32 * 1024,
        };
        let pool = crate::db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        assert_eq!(
            authenticated_request(&state, "bootstrap-key").await,
            StatusCode::UNAUTHORIZED,
            "a production state accepted the development bootstrap key"
        );
        let material = state.credentials.generate(Uuid::new_v4()).unwrap();

        let status = authenticated_request(&state, &material.plaintext).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    async fn authenticated_request(state: &Arc<AppState>, key: &str) -> StatusCode {
        routes::router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", format!("Bearer {key}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"company-default"}"#))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    async fn database_contains(pool: &Pool<Postgres>, needle: &str) -> bool {
        sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM (
                    SELECT to_jsonb(u)::text AS row_text FROM users u
                    UNION ALL SELECT to_jsonb(d)::text FROM devices d
                    UNION ALL SELECT to_jsonb(c)::text FROM clients c
                    UNION ALL SELECT to_jsonb(a)::text FROM agents a
                    UNION ALL SELECT to_jsonb(k)::text FROM credentials k
                ) all_rows
                WHERE position($1 in row_text) > 0
            )
            "#,
        )
        .bind(needle)
        .fetch_one(pool)
        .await
        .unwrap()
    }
}
