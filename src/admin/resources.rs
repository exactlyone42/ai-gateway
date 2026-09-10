use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::{Postgres, Row as _, Transaction};
use uuid::Uuid;

use crate::model_registry::GatewayOperation;

use super::{
    AdminContext, AdminError, AdminService, AuditEntry, append_audit, map_conflict, validate_name,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceKind {
    Users,
    Devices,
    Clients,
    Agents,
    Credentials,
    Providers,
    Models,
    ModelAliases,
    Policies,
    Quotas,
}

#[derive(Clone, Copy, Debug)]
pub struct ResourceTarget {
    pub kind: ResourceKind,
    pub id: Uuid,
}

impl ResourceTarget {
    pub fn new(kind: ResourceKind, id: Uuid) -> Self {
        Self { kind, id }
    }
}

impl FromStr for ResourceKind {
    type Err = AdminError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "users" => Ok(Self::Users),
            "devices" => Ok(Self::Devices),
            "clients" => Ok(Self::Clients),
            "agents" => Ok(Self::Agents),
            "credentials" => Ok(Self::Credentials),
            "providers" => Ok(Self::Providers),
            "models" => Ok(Self::Models),
            "model-aliases" => Ok(Self::ModelAliases),
            "policies" => Ok(Self::Policies),
            "quotas" => Ok(Self::Quotas),
            _ => Err(AdminError::NotFound),
        }
    }
}

impl ResourceKind {
    fn slug(self) -> &'static str {
        match self {
            Self::Users => "users",
            Self::Devices => "devices",
            Self::Clients => "clients",
            Self::Agents => "agents",
            Self::Credentials => "credentials",
            Self::Providers => "providers",
            Self::Models => "models",
            Self::ModelAliases => "model-aliases",
            Self::Policies => "policies",
            Self::Quotas => "quotas",
        }
    }

    fn singular(self) -> &'static str {
        match self {
            Self::Users => "user",
            Self::Devices => "device",
            Self::Clients => "client",
            Self::Agents => "agent",
            Self::Credentials => "credential",
            Self::Providers => "provider",
            Self::Models => "model",
            Self::ModelAliases => "model_alias",
            Self::Policies => "policy",
            Self::Quotas => "quota",
        }
    }

    fn is_identity(self) -> bool {
        matches!(
            self,
            Self::Users | Self::Devices | Self::Clients | Self::Agents
        )
    }

    fn hidden_fields(self) -> &'static [&'static str] {
        match self {
            Self::Credentials => &["secret_hash", "fingerprint"],
            _ => &[],
        }
    }
}

const LIST_RESOURCES: &str = r#"
    SELECT id, resource - $5::text[] AS resource
    FROM (
        SELECT 'users' AS kind, id, tenant_id, to_jsonb(users) AS resource FROM users
        UNION ALL SELECT 'devices', id, tenant_id, to_jsonb(devices) FROM devices
        UNION ALL SELECT 'clients', id, tenant_id, to_jsonb(clients) FROM clients
        UNION ALL SELECT 'agents', id, tenant_id, to_jsonb(agents) FROM agents
        UNION ALL SELECT 'credentials', id, tenant_id, to_jsonb(credentials) FROM credentials
        UNION ALL SELECT 'providers', id, tenant_id, to_jsonb(providers) FROM providers
        UNION ALL SELECT 'models', id, tenant_id, to_jsonb(models) FROM models
        UNION ALL SELECT 'model-aliases', id, tenant_id, to_jsonb(model_aliases) FROM model_aliases
        UNION ALL SELECT 'policies', id, tenant_id, to_jsonb(policies) FROM policies
        UNION ALL SELECT 'quotas', id, tenant_id, to_jsonb(quotas) FROM quotas
    ) resources
    WHERE kind = $1 AND tenant_id = $2 AND id > $3
    ORDER BY id LIMIT $4
"#;

const GET_RESOURCE: &str = r#"
    SELECT resource - $4::text[]
    FROM (
        SELECT 'users' AS kind, id, tenant_id, to_jsonb(users) AS resource FROM users
        UNION ALL SELECT 'devices', id, tenant_id, to_jsonb(devices) FROM devices
        UNION ALL SELECT 'clients', id, tenant_id, to_jsonb(clients) FROM clients
        UNION ALL SELECT 'agents', id, tenant_id, to_jsonb(agents) FROM agents
        UNION ALL SELECT 'credentials', id, tenant_id, to_jsonb(credentials) FROM credentials
        UNION ALL SELECT 'providers', id, tenant_id, to_jsonb(providers) FROM providers
        UNION ALL SELECT 'models', id, tenant_id, to_jsonb(models) FROM models
        UNION ALL SELECT 'model-aliases', id, tenant_id, to_jsonb(model_aliases) FROM model_aliases
        UNION ALL SELECT 'policies', id, tenant_id, to_jsonb(policies) FROM policies
        UNION ALL SELECT 'quotas', id, tenant_id, to_jsonb(quotas) FROM quotas
    ) resources
    WHERE kind = $1 AND tenant_id = $2 AND id = $3
"#;

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<u16>,
    pub after: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct DeleteQuery {
    pub version: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleInput {
    pub version: i64,
    pub state: String,
}

#[derive(Serialize)]
pub struct ResourcePage {
    pub items: Vec<Value>,
    pub next_after: Option<Uuid>,
}

#[derive(Serialize)]
pub struct ResourceMutationOutput {
    pub resource: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityCreate {
    display_name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialCreate {
    display_name: String,
    user_id: Option<Uuid>,
    device_id: Option<Uuid>,
    client_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderCreate {
    provider_key: String,
    display_name: String,
    provider_type: String,
    base_url: String,
    credential_ref: String,
    #[serde(default)]
    supported_operations: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelCreate {
    provider_id: Uuid,
    public_model_id: String,
    provider_model_id: String,
    display_name: Option<String>,
    description: Option<String>,
    quality_tier: Option<String>,
    context_window: Option<i64>,
    max_output_tokens: Option<i64>,
    #[serde(default)]
    supported_operations: Vec<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    supports_streaming: bool,
    #[serde(default)]
    supports_tools: bool,
    #[serde(default)]
    supports_reasoning: bool,
    #[serde(default)]
    supports_images: bool,
    #[serde(default)]
    supports_structured_output: bool,
    #[serde(default)]
    cost_metadata: serde_json::Map<String, Value>,
    latency_class: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelAliasCreate {
    operation: String,
    quality_tier: String,
    model_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyCreate {
    policy_key: String,
    revision: i64,
    user_id: Option<Uuid>,
    device_id: Option<Uuid>,
    client_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    credential_id: Option<Uuid>,
    configuration: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuotaCreate {
    quota_key: String,
    revision: i64,
    user_id: Option<Uuid>,
    device_id: Option<Uuid>,
    client_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    credential_id: Option<Uuid>,
    model_id: Option<Uuid>,
    provider_id: Option<Uuid>,
    configuration: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityPatch {
    version: i64,
    display_name: Option<String>,
    status: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialPatch {
    version: i64,
    display_name: Option<String>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderPatch {
    version: i64,
    display_name: Option<String>,
    base_url: Option<String>,
    credential_ref: Option<String>,
    supported_operations: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelPatch {
    version: i64,
    provider_model_id: Option<String>,
    display_name: Option<String>,
    description: Option<String>,
    quality_tier: Option<String>,
    context_window: Option<i64>,
    max_output_tokens: Option<i64>,
    supported_operations: Option<Vec<String>>,
    capabilities: Option<Vec<String>>,
    supports_streaming: Option<bool>,
    supports_tools: Option<bool>,
    supports_reasoning: Option<bool>,
    supports_images: Option<bool>,
    supports_structured_output: Option<bool>,
    cost_metadata: Option<serde_json::Map<String, Value>>,
    latency_class: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelAliasPatch {
    version: i64,
    operation: Option<String>,
    quality_tier: Option<String>,
    model_id: Option<Uuid>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigurationPatch {
    version: i64,
    configuration: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotateCredentialInput {
    pub version: i64,
    pub overlap_seconds: u64,
    pub expires_at: Option<DateTime<Utc>>,
}

impl AdminService {
    async fn lock_resource(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        context: &AdminContext,
        kind: ResourceKind,
        id: Uuid,
    ) -> Result<Value, AdminError> {
        let statement = match kind {
            ResourceKind::Users => {
                "SELECT to_jsonb(users) FROM users WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
            }
            ResourceKind::Devices => {
                "SELECT to_jsonb(devices) FROM devices WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
            }
            ResourceKind::Clients => {
                "SELECT to_jsonb(clients) FROM clients WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
            }
            ResourceKind::Agents => {
                "SELECT to_jsonb(agents) FROM agents WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
            }
            ResourceKind::Credentials => {
                "SELECT to_jsonb(credentials) - ARRAY['secret_hash', 'fingerprint']::text[] FROM credentials WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
            }
            ResourceKind::Providers => {
                "SELECT to_jsonb(providers) FROM providers WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
            }
            ResourceKind::Models => {
                "SELECT to_jsonb(models) FROM models WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
            }
            ResourceKind::ModelAliases => {
                "SELECT to_jsonb(model_aliases) FROM model_aliases WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
            }
            ResourceKind::Policies => {
                "SELECT to_jsonb(policies) FROM policies WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
            }
            ResourceKind::Quotas => {
                "SELECT to_jsonb(quotas) FROM quotas WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
            }
        };
        sqlx::query_scalar(statement)
            .bind(context.tenant_id)
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or(AdminError::NotFound)
    }

    pub async fn list_resources(
        &self,
        context: &AdminContext,
        kind: ResourceKind,
        query: ListQuery,
    ) -> Result<ResourcePage, AdminError> {
        let limit = query.limit.unwrap_or(50);
        if limit == 0 || limit > 100 {
            return Err(AdminError::InvalidRequest);
        }
        let after = query.after.unwrap_or(Uuid::nil());
        let rows = sqlx::query(LIST_RESOURCES)
            .bind(kind.slug())
            .bind(context.tenant_id)
            .bind(after)
            .bind(i64::from(limit) + 1)
            .bind(kind.hidden_fields())
            .fetch_all(&self.pool)
            .await?;
        let has_more = rows.len() > usize::from(limit);
        let visible = rows.into_iter().take(usize::from(limit));
        let mut items = Vec::with_capacity(usize::from(limit));
        let mut last_id = None;
        for row in visible {
            last_id = Some(row.try_get::<Uuid, _>("id")?);
            items.push(row.try_get("resource")?);
        }
        Ok(ResourcePage {
            items,
            next_after: has_more.then_some(last_id).flatten(),
        })
    }

    pub async fn get_resource(
        &self,
        context: &AdminContext,
        kind: ResourceKind,
        id: Uuid,
    ) -> Result<Value, AdminError> {
        sqlx::query_scalar(GET_RESOURCE)
            .bind(kind.slug())
            .bind(context.tenant_id)
            .bind(id)
            .bind(kind.hidden_fields())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(AdminError::NotFound)
    }

    pub async fn create_resource(
        &self,
        context: &AdminContext,
        kind: ResourceKind,
        payload: Value,
        source_ip: std::net::IpAddr,
        peer_ip: std::net::IpAddr,
        request_id: &str,
    ) -> Result<ResourceMutationOutput, AdminError> {
        let mut transaction = self.pool.begin().await?;
        let id = Uuid::new_v4();
        let (resource, secret) = match kind {
            ResourceKind::Users
            | ResourceKind::Devices
            | ResourceKind::Clients
            | ResourceKind::Agents => {
                let input: IdentityCreate = decode(payload)?;
                validate_name(&input.display_name)?;
                let statement = match kind {
                    ResourceKind::Users => {
                        "INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, $3) RETURNING to_jsonb(users)"
                    }
                    ResourceKind::Devices => {
                        "INSERT INTO devices (id, tenant_id, display_name) VALUES ($1, $2, $3) RETURNING to_jsonb(devices)"
                    }
                    ResourceKind::Clients => {
                        "INSERT INTO clients (id, tenant_id, display_name) VALUES ($1, $2, $3) RETURNING to_jsonb(clients)"
                    }
                    ResourceKind::Agents => {
                        "INSERT INTO agents (id, tenant_id, display_name) VALUES ($1, $2, $3) RETURNING to_jsonb(agents)"
                    }
                    _ => return Err(AdminError::Unavailable),
                };
                let value: Value = sqlx::query_scalar(statement)
                    .bind(id)
                    .bind(context.tenant_id)
                    .bind(input.display_name)
                    .fetch_one(&mut *transaction)
                    .await?;
                (value, None)
            }
            ResourceKind::Credentials => {
                let input: CredentialCreate = decode(payload)?;
                validate_name(&input.display_name)?;
                if [
                    input.user_id,
                    input.device_id,
                    input.client_id,
                    input.agent_id,
                ]
                .into_iter()
                .all(|value| value.is_none())
                {
                    return Err(AdminError::InvalidRequest);
                }
                let material = self.credentials.generate(context.tenant_id)?;
                let value: Value = sqlx::query_scalar(
                    r#"
                    INSERT INTO credentials (
                        id, tenant_id, "type", display_name, secret_hash, hash_key_id,
                        key_prefix, fingerprint, user_id, device_id, client_id, agent_id, expires_at
                    ) VALUES ($1, $2, 'client_api_key', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                    RETURNING to_jsonb(credentials) - ARRAY['secret_hash', 'fingerprint']::text[]
                    "#,
                )
                .bind(id)
                .bind(context.tenant_id)
                .bind(input.display_name)
                .bind(material.secret_hash.as_slice())
                .bind(&material.hash_key_id)
                .bind(&material.key_prefix)
                .bind(material.fingerprint.as_slice())
                .bind(input.user_id)
                .bind(input.device_id)
                .bind(input.client_id)
                .bind(input.agent_id)
                .bind(input.expires_at)
                .fetch_one(&mut *transaction)
                .await?;
                (value, Some(material.plaintext))
            }
            ResourceKind::Providers => {
                let input: ProviderCreate = decode(payload)?;
                validate_name(&input.display_name)?;
                validate_operations(&input.supported_operations)?;
                let value: Value = sqlx::query_scalar(
                    r#"
                    INSERT INTO providers (
                        id, tenant_id, provider_key, display_name, provider_type, base_url,
                        credential_ref, supported_operations
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                    RETURNING to_jsonb(providers)
                    "#,
                )
                .bind(id)
                .bind(context.tenant_id)
                .bind(input.provider_key)
                .bind(input.display_name)
                .bind(input.provider_type)
                .bind(input.base_url)
                .bind(input.credential_ref)
                .bind(input.supported_operations)
                .fetch_one(&mut *transaction)
                .await?;
                (value, None)
            }
            ResourceKind::Models => {
                let input: ModelCreate = decode(payload)?;
                let display_name = input
                    .display_name
                    .unwrap_or_else(|| input.public_model_id.clone());
                validate_name(&display_name)?;
                validate_model_fields(
                    &input.public_model_id,
                    &input.provider_model_id,
                    input.description.as_deref(),
                    input.latency_class.as_deref(),
                    &input.supported_operations,
                    &input.cost_metadata,
                )?;
                let value: Value = sqlx::query_scalar(
                    r#"
                    INSERT INTO models (
                        id, tenant_id, provider_id, public_model_id, provider_model_id,
                        display_name, description,
                        quality_tier, context_window, max_output_tokens, supported_operations, capabilities,
                        supports_streaming, supports_tools, supports_reasoning, supports_images,
                        supports_structured_output, cost_metadata, latency_class
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)
                    RETURNING to_jsonb(models)
                    "#,
                )
                .bind(id)
                .bind(context.tenant_id)
                .bind(input.provider_id)
                .bind(input.public_model_id)
                .bind(input.provider_model_id)
                .bind(display_name)
                .bind(input.description)
                .bind(input.quality_tier)
                .bind(input.context_window)
                .bind(input.max_output_tokens)
                .bind(input.supported_operations)
                .bind(json!(input.capabilities))
                .bind(input.supports_streaming)
                .bind(input.supports_tools)
                .bind(input.supports_reasoning)
                .bind(input.supports_images)
                .bind(input.supports_structured_output)
                .bind(Value::Object(input.cost_metadata))
                .bind(input.latency_class)
                .fetch_one(&mut *transaction)
                .await?;
                (value, None)
            }
            ResourceKind::ModelAliases => {
                let input: ModelAliasCreate = decode(payload)?;
                let value: Value = sqlx::query_scalar(
                    r#"
                    INSERT INTO model_aliases (
                        id, tenant_id, operation, quality_tier, model_id
                    ) VALUES ($1, $2, $3, $4, $5)
                    RETURNING to_jsonb(model_aliases)
                    "#,
                )
                .bind(id)
                .bind(context.tenant_id)
                .bind(input.operation)
                .bind(input.quality_tier)
                .bind(input.model_id)
                .fetch_one(&mut *transaction)
                .await?;
                (value, None)
            }
            ResourceKind::Policies => {
                let input: PolicyCreate = decode(payload)?;
                let value: Value = sqlx::query_scalar(
                    r#"
                    INSERT INTO policies (
                        id, tenant_id, policy_key, revision, user_id, device_id, client_id,
                        agent_id, credential_id, configuration
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                    RETURNING to_jsonb(policies)
                    "#,
                )
                .bind(id)
                .bind(context.tenant_id)
                .bind(input.policy_key)
                .bind(input.revision)
                .bind(input.user_id)
                .bind(input.device_id)
                .bind(input.client_id)
                .bind(input.agent_id)
                .bind(input.credential_id)
                .bind(Value::Object(input.configuration))
                .fetch_one(&mut *transaction)
                .await?;
                (value, None)
            }
            ResourceKind::Quotas => {
                let input: QuotaCreate = decode(payload)?;
                let value: Value = sqlx::query_scalar(
                    r#"
                    INSERT INTO quotas (
                        id, tenant_id, quota_key, revision, user_id, device_id, client_id,
                        agent_id, credential_id, model_id, provider_id, configuration
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                    RETURNING to_jsonb(quotas)
                    "#,
                )
                .bind(id)
                .bind(context.tenant_id)
                .bind(input.quota_key)
                .bind(input.revision)
                .bind(input.user_id)
                .bind(input.device_id)
                .bind(input.client_id)
                .bind(input.agent_id)
                .bind(input.credential_id)
                .bind(input.model_id)
                .bind(input.provider_id)
                .bind(Value::Object(input.configuration))
                .fetch_one(&mut *transaction)
                .await?;
                (value, None)
            }
        };
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: &format!("admin.{}.create", kind.singular()),
                target_type: kind.singular(),
                target_id: Some(id),
                before: None,
                after: Some(resource.clone()),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(ResourceMutationOutput { resource, secret })
    }

    pub async fn update_resource(
        &self,
        context: &AdminContext,
        target: ResourceTarget,
        payload: Value,
        source_ip: std::net::IpAddr,
        peer_ip: std::net::IpAddr,
        request_id: &str,
    ) -> Result<ResourceMutationOutput, AdminError> {
        let ResourceTarget { kind, id } = target;
        let valid_version = payload
            .get("version")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .is_some();
        if !valid_version {
            return Err(AdminError::InvalidRequest);
        }
        let lifecycle = self.credentials.lifecycle_write().await;
        let mut transaction = self.pool.begin().await?;
        let before = self
            .lock_resource(&mut transaction, context, kind, id)
            .await?;
        let resource: Value = match kind {
            ResourceKind::Users
            | ResourceKind::Devices
            | ResourceKind::Clients
            | ResourceKind::Agents => {
                let input: IdentityPatch = decode(payload)?;
                if input.display_name.is_none() && input.status.is_none() {
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
                let statement = match kind {
                    ResourceKind::Users => {
                        "UPDATE users SET display_name = COALESCE($4, display_name), status = COALESCE($5, status), version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 RETURNING to_jsonb(users)"
                    }
                    ResourceKind::Devices => {
                        "UPDATE devices SET display_name = COALESCE($4, display_name), status = COALESCE($5, status), version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 RETURNING to_jsonb(devices)"
                    }
                    ResourceKind::Clients => {
                        "UPDATE clients SET display_name = COALESCE($4, display_name), status = COALESCE($5, status), version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 RETURNING to_jsonb(clients)"
                    }
                    ResourceKind::Agents => {
                        "UPDATE agents SET display_name = COALESCE($4, display_name), status = COALESCE($5, status), version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 RETURNING to_jsonb(agents)"
                    }
                    _ => return Err(AdminError::Unavailable),
                };
                let resource = sqlx::query_scalar(statement)
                    .bind(context.tenant_id)
                    .bind(id)
                    .bind(input.version)
                    .bind(input.display_name.as_deref())
                    .bind(input.status.as_deref())
                    .fetch_optional(&mut *transaction)
                    .await?
                    .ok_or(AdminError::Conflict)?;
                if input.status.as_deref() == Some("disabled") {
                    sqlx::query(
                        r#"
                        UPDATE credentials
                        SET status = 'revoked', revoked_at = now(),
                            version = version + 1, updated_at = now()
                        WHERE tenant_id = $1 AND status IN ('active', 'disabled') AND (
                            ($2 = 'users' AND user_id = $3) OR
                            ($2 = 'devices' AND device_id = $3) OR
                            ($2 = 'clients' AND client_id = $3) OR
                            ($2 = 'agents' AND agent_id = $3)
                        )
                        "#,
                    )
                    .bind(context.tenant_id)
                    .bind(kind.slug())
                    .bind(id)
                    .execute(&mut *transaction)
                    .await?;
                }
                resource
            }
            ResourceKind::Credentials => {
                let input: CredentialPatch = decode(payload)?;
                if input.display_name.is_none() && input.expires_at.is_none() {
                    return Err(AdminError::InvalidRequest);
                }
                if let Some(name) = input.display_name.as_deref() {
                    validate_name(name)?;
                }
                sqlx::query_scalar(
                    r#"
                    UPDATE credentials
                    SET display_name = COALESCE($4, display_name),
                        expires_at = COALESCE($5, expires_at), version = version + 1, updated_at = now()
                    WHERE tenant_id = $1 AND id = $2 AND version = $3
                      AND status IN ('active', 'disabled') AND revoked_at IS NULL
                    RETURNING to_jsonb(credentials) - ARRAY['secret_hash', 'fingerprint']::text[]
                    "#,
                )
                .bind(context.tenant_id)
                .bind(id)
                .bind(input.version)
                .bind(input.display_name)
                .bind(input.expires_at)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or(AdminError::Conflict)?
            }
            ResourceKind::Providers => {
                let input: ProviderPatch = decode(payload)?;
                if let Some(display_name) = input.display_name.as_deref() {
                    validate_name(display_name)?;
                }
                if let Some(operations) = input.supported_operations.as_deref() {
                    validate_operations(operations)?;
                }
                sqlx::query_scalar(
                    r#"
                    UPDATE providers
                    SET display_name = COALESCE($4, display_name), base_url = COALESCE($5, base_url),
                        credential_ref = COALESCE($6, credential_ref),
                        supported_operations = COALESCE($7, supported_operations),
                        version = version + 1, updated_at = now()
                    WHERE tenant_id = $1 AND id = $2 AND version = $3
                      AND lifecycle_state IN ('draft', 'disabled')
                    RETURNING to_jsonb(providers)
                    "#,
                )
                .bind(context.tenant_id)
                .bind(id)
                .bind(input.version)
                .bind(input.display_name)
                .bind(input.base_url)
                .bind(input.credential_ref)
                .bind(input.supported_operations)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or(AdminError::Conflict)?
            }
            ResourceKind::Models => {
                let input: ModelPatch = decode(payload)?;
                if let Some(display_name) = input.display_name.as_deref() {
                    validate_name(display_name)?;
                }
                validate_optional_model_fields(&input)?;
                sqlx::query_scalar(
                    r#"
                    UPDATE models SET
                        provider_model_id = COALESCE($4, provider_model_id),
                        display_name = COALESCE($5, display_name),
                        description = COALESCE($6, description),
                        quality_tier = COALESCE($7, quality_tier),
                        context_window = COALESCE($8, context_window),
                        max_output_tokens = COALESCE($9, max_output_tokens),
                        supported_operations = COALESCE($10, supported_operations),
                        capabilities = COALESCE($11, capabilities),
                        supports_streaming = COALESCE($12, supports_streaming),
                        supports_tools = COALESCE($13, supports_tools),
                        supports_reasoning = COALESCE($14, supports_reasoning),
                        supports_images = COALESCE($15, supports_images),
                        supports_structured_output = COALESCE($16, supports_structured_output),
                        cost_metadata = COALESCE($17, cost_metadata),
                        latency_class = COALESCE($18, latency_class),
                        version = version + 1, updated_at = now()
                    WHERE tenant_id = $1 AND id = $2 AND version = $3
                      AND lifecycle_state IN ('draft', 'disabled')
                    RETURNING to_jsonb(models)
                    "#,
                )
                .bind(context.tenant_id)
                .bind(id)
                .bind(input.version)
                .bind(input.provider_model_id)
                .bind(input.display_name)
                .bind(input.description)
                .bind(input.quality_tier)
                .bind(input.context_window)
                .bind(input.max_output_tokens)
                .bind(input.supported_operations)
                .bind(input.capabilities.map(|value| json!(value)))
                .bind(input.supports_streaming)
                .bind(input.supports_tools)
                .bind(input.supports_reasoning)
                .bind(input.supports_images)
                .bind(input.supports_structured_output)
                .bind(input.cost_metadata.map(Value::Object))
                .bind(input.latency_class)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or(AdminError::Conflict)?
            }
            ResourceKind::Policies | ResourceKind::Quotas => {
                let input: ConfigurationPatch = decode(payload)?;
                let statement = match kind {
                    ResourceKind::Policies => {
                        "UPDATE policies SET configuration = $4, version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state = 'draft' RETURNING to_jsonb(policies)"
                    }
                    ResourceKind::Quotas => {
                        "UPDATE quotas SET configuration = $4, version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state = 'draft' RETURNING to_jsonb(quotas)"
                    }
                    _ => return Err(AdminError::Unavailable),
                };
                sqlx::query_scalar(statement)
                    .bind(context.tenant_id)
                    .bind(id)
                    .bind(input.version)
                    .bind(Value::Object(input.configuration))
                    .fetch_optional(&mut *transaction)
                    .await?
                    .ok_or(AdminError::Conflict)?
            }
            ResourceKind::ModelAliases => {
                let input: ModelAliasPatch = decode(payload)?;
                sqlx::query_scalar(
                    r#"
                    UPDATE model_aliases SET
                        operation = COALESCE($4, operation),
                        quality_tier = COALESCE($5, quality_tier),
                        model_id = COALESCE($6, model_id),
                        version = version + 1, updated_at = now()
                    WHERE tenant_id = $1 AND id = $2 AND version = $3
                      AND lifecycle_state IN ('draft', 'disabled')
                    RETURNING to_jsonb(model_aliases)
                    "#,
                )
                .bind(context.tenant_id)
                .bind(id)
                .bind(input.version)
                .bind(input.operation)
                .bind(input.quality_tier)
                .bind(input.model_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or(AdminError::Conflict)?
            }
        };
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: &format!("admin.{}.update", kind.singular()),
                target_type: kind.singular(),
                target_id: Some(id),
                before: Some(before),
                after: Some(resource.clone()),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        if kind.is_identity() || kind == ResourceKind::Credentials {
            self.credentials.clear_tenant_cache(context.tenant_id)?;
        }
        drop(lifecycle);
        Ok(ResourceMutationOutput {
            resource,
            secret: None,
        })
    }

    pub async fn delete_resource(
        &self,
        context: &AdminContext,
        target: ResourceTarget,
        version: i64,
        source_ip: std::net::IpAddr,
        peer_ip: std::net::IpAddr,
        request_id: &str,
    ) -> Result<ResourceMutationOutput, AdminError> {
        let ResourceTarget { kind, id } = target;
        if version <= 0 {
            return Err(AdminError::InvalidRequest);
        }
        let payload = match kind {
            ResourceKind::Users
            | ResourceKind::Devices
            | ResourceKind::Clients
            | ResourceKind::Agents => json!({"version": version, "status": "disabled"}),
            ResourceKind::Credentials => {
                return self
                    .revoke_credential(context, id, version, source_ip, peer_ip, request_id)
                    .await;
            }
            ResourceKind::Providers
            | ResourceKind::Models
            | ResourceKind::ModelAliases
            | ResourceKind::Policies
            | ResourceKind::Quotas => {
                return self
                    .archive_draft(context, target, version, source_ip, peer_ip, request_id)
                    .await;
            }
        };
        self.update_resource(context, target, payload, source_ip, peer_ip, request_id)
            .await
    }

    pub async fn set_registry_lifecycle(
        &self,
        context: &AdminContext,
        target: ResourceTarget,
        input: LifecycleInput,
        source_ip: std::net::IpAddr,
        peer_ip: std::net::IpAddr,
        request_id: &str,
    ) -> Result<ResourceMutationOutput, AdminError> {
        let ResourceTarget { kind, id } = target;
        if context.role != super::AdminRole::Admin {
            return Err(AdminError::Forbidden);
        }
        if input.version <= 0 || !matches!(input.state.as_str(), "active" | "disabled") {
            return Err(AdminError::InvalidRequest);
        }
        if !matches!(
            kind,
            ResourceKind::Providers | ResourceKind::Models | ResourceKind::ModelAliases
        ) {
            return Err(AdminError::InvalidRequest);
        }

        let mut transaction = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
            .bind(context.tenant_id)
            .execute(&mut *transaction)
            .await?;
        let before = self
            .lock_resource(&mut transaction, context, kind, id)
            .await?;
        let cascaded_before = if input.state == "disabled" {
            load_registry_cascade(&mut transaction, context.tenant_id, kind, id).await?
        } else if kind == ResourceKind::ModelAliases {
            load_alias_replacement(&mut transaction, context.tenant_id, id).await?
        } else {
            Vec::new()
        };
        if kind == ResourceKind::Providers && input.state == "active" {
            let operations = before["supported_operations"]
                .as_array()
                .ok_or(AdminError::InvalidRequest)?
                .iter()
                .map(|value| value.as_str().map(ToOwned::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or(AdminError::InvalidRequest)?;
            let trusted = self.provider_profiles.as_ref().is_some_and(|profiles| {
                profiles.validates(
                    context.tenant_id,
                    before["credential_ref"].as_str().unwrap_or_default(),
                    before["provider_key"].as_str().unwrap_or_default(),
                    before["base_url"].as_str().unwrap_or_default(),
                    &operations,
                )
            });
            if !trusted {
                return Err(AdminError::InvalidRequest);
            }
        }
        if kind == ResourceKind::ModelAliases && input.state == "active" {
            sqlx::query(
                r#"
                UPDATE model_aliases SET lifecycle_state = 'disabled',
                    version = version + 1, updated_at = now()
                WHERE tenant_id = $1
                  AND operation = $2 AND quality_tier = $3
                  AND id <> $4 AND lifecycle_state = 'active'
                "#,
            )
            .bind(context.tenant_id)
            .bind(
                before["operation"]
                    .as_str()
                    .ok_or(AdminError::InvalidRequest)?,
            )
            .bind(
                before["quality_tier"]
                    .as_str()
                    .ok_or(AdminError::InvalidRequest)?,
            )
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        }

        let statement = match (kind, input.state.as_str()) {
            (ResourceKind::Providers, "active") => {
                "UPDATE providers SET lifecycle_state = 'active', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state IN ('draft', 'disabled') RETURNING to_jsonb(providers)"
            }
            (ResourceKind::Providers, "disabled") => {
                r#"
                WITH disabled_aliases AS (
                    UPDATE model_aliases a SET lifecycle_state = 'disabled',
                        version = a.version + 1, updated_at = now()
                    FROM models m
                    WHERE (m.tenant_id, m.id) = (a.tenant_id, a.model_id)
                      AND m.tenant_id = $1 AND m.provider_id = $2
                      AND a.lifecycle_state = 'active'
                ), disabled_models AS (
                    UPDATE models SET lifecycle_state = 'disabled',
                        version = version + 1, updated_at = now()
                    WHERE tenant_id = $1 AND provider_id = $2 AND lifecycle_state = 'active'
                )
                UPDATE providers SET lifecycle_state = 'disabled',
                    version = version + 1, updated_at = now()
                WHERE tenant_id = $1 AND id = $2 AND version = $3
                  AND lifecycle_state = 'active'
                RETURNING to_jsonb(providers)
                "#
            }
            (ResourceKind::Models, "active") => {
                r#"
                UPDATE models m SET lifecycle_state = 'active',
                    version = m.version + 1, updated_at = now()
                FROM providers p
                WHERE (p.tenant_id, p.id) = (m.tenant_id, m.provider_id)
                  AND m.tenant_id = $1 AND m.id = $2 AND m.version = $3
                  AND m.lifecycle_state IN ('draft', 'disabled')
                  AND p.lifecycle_state = 'active'
                  AND valid_gateway_operations(m.supported_operations)
                  AND m.supported_operations <@ p.supported_operations
                RETURNING to_jsonb(m)
                "#
            }
            (ResourceKind::Models, "disabled") => {
                r#"
                WITH disabled_aliases AS (
                    UPDATE model_aliases SET lifecycle_state = 'disabled',
                        version = version + 1, updated_at = now()
                    WHERE tenant_id = $1 AND model_id = $2 AND lifecycle_state = 'active'
                )
                UPDATE models SET lifecycle_state = 'disabled',
                    version = version + 1, updated_at = now()
                WHERE tenant_id = $1 AND id = $2 AND version = $3
                  AND lifecycle_state = 'active'
                RETURNING to_jsonb(models)
                "#
            }
            (ResourceKind::ModelAliases, "active") => {
                r#"
                UPDATE model_aliases a SET lifecycle_state = 'active',
                    version = a.version + 1, updated_at = now()
                FROM models m
                JOIN providers p ON (p.tenant_id, p.id) = (m.tenant_id, m.provider_id)
                WHERE (m.tenant_id, m.id) = (a.tenant_id, a.model_id)
                  AND a.tenant_id = $1 AND a.id = $2 AND a.version = $3
                  AND a.lifecycle_state IN ('draft', 'disabled')
                  AND m.lifecycle_state = 'active' AND p.lifecycle_state = 'active'
                  AND a.operation = ANY(p.supported_operations)
                  AND a.operation = ANY(m.supported_operations)
                RETURNING to_jsonb(a)
                "#
            }
            (ResourceKind::ModelAliases, "disabled") => {
                "UPDATE model_aliases SET lifecycle_state = 'disabled', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state = 'active' RETURNING to_jsonb(model_aliases)"
            }
            _ => return Err(AdminError::InvalidRequest),
        };
        let after: Value = sqlx::query_scalar(statement)
            .bind(context.tenant_id)
            .bind(id)
            .bind(input.version)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_conflict)?
            .ok_or(AdminError::Conflict)?;
        append_registry_cascade_audits(
            &mut transaction,
            context,
            &cascaded_before,
            source_ip,
            peer_ip,
            request_id,
        )
        .await?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: &format!("admin.{}.{}", kind.singular(), input.state),
                target_type: kind.singular(),
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
        Ok(ResourceMutationOutput {
            resource: after,
            secret: None,
        })
    }

    pub async fn rotate_credential(
        &self,
        context: &AdminContext,
        id: Uuid,
        input: RotateCredentialInput,
        source_ip: std::net::IpAddr,
        peer_ip: std::net::IpAddr,
        request_id: &str,
    ) -> Result<ResourceMutationOutput, AdminError> {
        if input.version <= 0 || !(1..=900).contains(&input.overlap_seconds) {
            return Err(AdminError::InvalidRequest);
        }
        let lifecycle = self.credentials.lifecycle_write().await;
        let material = self.credentials.generate(context.tenant_id)?;
        let new_id = Uuid::new_v4();
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            r#"
            WITH old AS (
                SELECT * FROM credentials
                WHERE tenant_id = $1 AND id = $2 AND version = $3
                  AND status = 'active' AND revoked_at IS NULL
                  AND (expires_at IS NULL OR expires_at > now())
                FOR UPDATE
            ), shortened AS (
                UPDATE credentials current
                SET expires_at = LEAST(
                    COALESCE(current.expires_at, now() + $4 * interval '1 second'),
                    now() + $4 * interval '1 second'
                ), version = current.version + 1, updated_at = now()
                FROM old
                WHERE current.tenant_id = old.tenant_id AND current.id = old.id
                RETURNING old.*
            ), inserted AS (
                INSERT INTO credentials (
                    id, tenant_id, "type", display_name, secret_hash, hash_key_id, key_prefix,
                    fingerprint, user_id, device_id, client_id, agent_id, expires_at, rotated_from_id
                )
                SELECT $5, $1, "type", display_name, $6, $7, $8, $9,
                       user_id, device_id, client_id, agent_id,
                       COALESCE($10, expires_at), $2
                FROM shortened
                RETURNING *
            )
            SELECT to_jsonb(shortened) - ARRAY['secret_hash', 'fingerprint']::text[] AS before,
                   to_jsonb(inserted) - ARRAY['secret_hash', 'fingerprint']::text[] AS after
            FROM shortened, inserted
            "#,
        )
        .bind(context.tenant_id)
        .bind(id)
        .bind(input.version)
        .bind(i64::try_from(input.overlap_seconds).map_err(|_| AdminError::InvalidRequest)?)
        .bind(new_id)
        .bind(material.secret_hash.as_slice())
        .bind(&material.hash_key_id)
        .bind(&material.key_prefix)
        .bind(material.fingerprint.as_slice())
        .bind(input.expires_at)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(AdminError::Conflict)?;
        let before: Value = row.try_get("before")?;
        let after: Value = row.try_get("after")?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: "admin.credential.rotate",
                target_type: "credential",
                target_id: Some(new_id),
                before: Some(before),
                after: Some(after.clone()),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        self.credentials.clear_tenant_cache(context.tenant_id)?;
        drop(lifecycle);
        Ok(ResourceMutationOutput {
            resource: after,
            secret: Some(material.plaintext),
        })
    }

    async fn revoke_credential(
        &self,
        context: &AdminContext,
        id: Uuid,
        version: i64,
        source_ip: std::net::IpAddr,
        peer_ip: std::net::IpAddr,
        request_id: &str,
    ) -> Result<ResourceMutationOutput, AdminError> {
        let lifecycle = self.credentials.lifecycle_write().await;
        let mut transaction = self.pool.begin().await?;
        let before = self
            .lock_resource(&mut transaction, context, ResourceKind::Credentials, id)
            .await?;
        let after: Value = sqlx::query_scalar(
            r#"
            UPDATE credentials
            SET status = 'revoked', revoked_at = now(), version = version + 1, updated_at = now()
            WHERE tenant_id = $1 AND id = $2 AND version = $3
              AND status IN ('active', 'disabled')
            RETURNING to_jsonb(credentials) - ARRAY['secret_hash', 'fingerprint']::text[]
            "#,
        )
        .bind(context.tenant_id)
        .bind(id)
        .bind(version)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(AdminError::Conflict)?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: "admin.credential.revoke",
                target_type: "credential",
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
        self.credentials.clear_tenant_cache(context.tenant_id)?;
        drop(lifecycle);
        Ok(ResourceMutationOutput {
            resource: after,
            secret: None,
        })
    }

    async fn archive_draft(
        &self,
        context: &AdminContext,
        target: ResourceTarget,
        version: i64,
        source_ip: std::net::IpAddr,
        peer_ip: std::net::IpAddr,
        request_id: &str,
    ) -> Result<ResourceMutationOutput, AdminError> {
        let ResourceTarget { kind, id } = target;
        let mut transaction = self.pool.begin().await?;
        let before = self
            .lock_resource(&mut transaction, context, kind, id)
            .await?;
        let statement = match kind {
            ResourceKind::Providers => {
                "UPDATE providers SET lifecycle_state = 'archived', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state IN ('draft', 'disabled') RETURNING to_jsonb(providers)"
            }
            ResourceKind::Models => {
                "UPDATE models SET lifecycle_state = 'archived', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state IN ('draft', 'disabled') RETURNING to_jsonb(models)"
            }
            ResourceKind::ModelAliases => {
                "UPDATE model_aliases SET lifecycle_state = 'archived', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state IN ('draft', 'disabled') RETURNING to_jsonb(model_aliases)"
            }
            ResourceKind::Policies => {
                "UPDATE policies SET lifecycle_state = 'archived', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state = 'draft' RETURNING to_jsonb(policies)"
            }
            ResourceKind::Quotas => {
                "UPDATE quotas SET lifecycle_state = 'archived', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state = 'draft' RETURNING to_jsonb(quotas)"
            }
            _ => return Err(AdminError::InvalidRequest),
        };
        let after: Value = sqlx::query_scalar(statement)
            .bind(context.tenant_id)
            .bind(id)
            .bind(version)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(AdminError::Conflict)?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: &format!("admin.{}.archive", kind.singular()),
                target_type: kind.singular(),
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
        Ok(ResourceMutationOutput {
            resource: after,
            secret: None,
        })
    }
}

fn decode<T: DeserializeOwned>(payload: Value) -> Result<T, AdminError> {
    serde_json::from_value(payload).map_err(|_| AdminError::InvalidRequest)
}

fn validate_operations(operations: &[String]) -> Result<(), AdminError> {
    if operations.is_empty()
        || operations
            .iter()
            .any(|operation| GatewayOperation::parse(operation).is_none())
    {
        return Err(AdminError::InvalidRequest);
    }
    Ok(())
}

fn validate_model_fields(
    public_model_id: &str,
    provider_model_id: &str,
    description: Option<&str>,
    latency_class: Option<&str>,
    operations: &[String],
    cost_metadata: &serde_json::Map<String, Value>,
) -> Result<(), AdminError> {
    if public_model_id.trim() != public_model_id
        || provider_model_id.trim() != provider_model_id
        || public_model_id.is_empty()
        || provider_model_id.is_empty()
        || public_model_id.len() > 255
        || provider_model_id.len() > 255
        || public_model_id.starts_with("quality:")
        || description.is_some_and(|value| value.trim().is_empty() || value.len() > 1024)
        || latency_class.is_some_and(|value| value.len() > 64)
        || serde_json::to_vec(cost_metadata).map_or(true, |value| value.len() > 4096)
    {
        return Err(AdminError::InvalidRequest);
    }
    validate_operations(operations)
}

fn validate_optional_model_fields(input: &ModelPatch) -> Result<(), AdminError> {
    if input
        .provider_model_id
        .as_deref()
        .is_some_and(|value| value.trim() != value || value.is_empty() || value.len() > 255)
        || input
            .description
            .as_deref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > 1024)
        || input
            .latency_class
            .as_deref()
            .is_some_and(|value| value.len() > 64)
        || input
            .cost_metadata
            .as_ref()
            .is_some_and(|value| serde_json::to_vec(value).map_or(true, |bytes| bytes.len() > 4096))
    {
        return Err(AdminError::InvalidRequest);
    }
    if let Some(operations) = input.supported_operations.as_deref() {
        validate_operations(operations)?;
    }
    Ok(())
}

async fn load_registry_cascade(
    transaction: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    kind: ResourceKind,
    id: Uuid,
) -> Result<Vec<Value>, AdminError> {
    if !matches!(kind, ResourceKind::Providers | ResourceKind::Models) {
        return Ok(Vec::new());
    }
    let rows: Value = sqlx::query_scalar(
        r#"
        SELECT COALESCE(jsonb_agg(item ORDER BY item->>'target_type', item->>'id'), '[]'::jsonb)
        FROM (
            SELECT jsonb_build_object('target_type', 'model', 'id', m.id, 'before', to_jsonb(m)) AS item
            FROM models m
            WHERE $2 = 'provider' AND m.tenant_id = $1 AND m.provider_id = $3
              AND m.lifecycle_state = 'active'
            UNION ALL
            SELECT jsonb_build_object('target_type', 'model_alias', 'id', a.id, 'before', to_jsonb(a))
            FROM model_aliases a
            JOIN models m ON (m.tenant_id, m.id) = (a.tenant_id, a.model_id)
            WHERE a.tenant_id = $1 AND a.lifecycle_state = 'active'
              AND (($2 = 'provider' AND m.provider_id = $3) OR ($2 = 'model' AND m.id = $3))
        ) cascade_rows
        "#,
    )
    .bind(tenant_id)
    .bind(kind.singular())
    .bind(id)
    .fetch_one(&mut **transaction)
    .await?;
    Ok(rows.as_array().cloned().unwrap_or_default())
}

async fn load_alias_replacement(
    transaction: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Vec<Value>, AdminError> {
    let rows: Value = sqlx::query_scalar(
        r#"
        SELECT COALESCE(jsonb_agg(jsonb_build_object(
            'target_type', 'model_alias', 'id', current.id, 'before', to_jsonb(current)
        )), '[]'::jsonb)
        FROM model_aliases target
        JOIN model_aliases current
          ON current.tenant_id = target.tenant_id
         AND current.operation = target.operation
         AND current.quality_tier = target.quality_tier
         AND current.id <> target.id
        WHERE target.tenant_id = $1 AND target.id = $2
          AND current.lifecycle_state = 'active'
        "#,
    )
    .bind(tenant_id)
    .bind(id)
    .fetch_one(&mut **transaction)
    .await?;
    Ok(rows.as_array().cloned().unwrap_or_default())
}

async fn append_registry_cascade_audits(
    transaction: &mut Transaction<'_, Postgres>,
    context: &AdminContext,
    cascaded: &[Value],
    source_ip: std::net::IpAddr,
    peer_ip: std::net::IpAddr,
    request_id: &str,
) -> Result<(), AdminError> {
    if cascaded.is_empty() {
        return Ok(());
    }
    let audit_ids = (0..cascaded.len())
        .map(|_| Uuid::new_v4())
        .collect::<Vec<_>>();
    sqlx::query(
        r#"
        WITH ids AS (
            SELECT id, ordinal FROM unnest($1::uuid[]) WITH ORDINALITY AS value(id, ordinal)
        ), entries AS (
            SELECT value, ordinal
            FROM jsonb_array_elements($2::jsonb) WITH ORDINALITY AS item(value, ordinal)
        )
        INSERT INTO audit_logs (
            id, tenant_id, actor_admin_id, actor_role, action, target_type,
            target_id, before_state, after_state, occurred_at, source_ip, peer_ip, request_id
        )
        SELECT ids.id, $3, $4, $5,
               'admin.' || (entries.value->>'target_type') || '.disabled',
               entries.value->>'target_type', (entries.value->>'id')::uuid,
               entries.value->'before',
               (entries.value->'before') || jsonb_build_object(
                   'lifecycle_state', 'disabled',
                   'version', ((entries.value->'before'->>'version')::bigint + 1)
               ),
               now(), $6::inet, $7::inet, $8
        FROM entries JOIN ids USING (ordinal)
        "#,
    )
    .bind(audit_ids)
    .bind(Value::Array(cascaded.to_vec()))
    .bind(context.tenant_id)
    .bind(context.account_id)
    .bind(context.role.as_str())
    .bind(source_ip.to_string())
    .bind(peer_ip.to_string())
    .bind(request_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{net::IpAddr, str::FromStr as _, sync::Arc, time::Duration};

    use serde_json::json;
    use sqlx::{PgPool, Row as _};
    use uuid::Uuid;

    use super::{
        LifecycleInput, ListQuery, ResourceKind, ResourceTarget, RotateCredentialInput,
        validate_model_fields,
    };
    use crate::{
        admin::{AdminContext, AdminError, AdminRole, AdminService, AdminServiceConfig},
        identity::{CredentialService, IdentityError},
        model_registry::{GatewayOperation, ProviderProfile, ProviderProfiles},
    };

    fn service(pool: PgPool) -> AdminService {
        let hash_key = b"resource-test-credential-hash-key-at-least-32".to_vec();
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
                bootstrap_token: None,
                bootstrap_tenant_id: None,
                session_ttl: Duration::from_secs(900),
            },
        )
    }

    fn provider_profiles(tenant_id: Uuid) -> ProviderProfiles {
        ProviderProfiles::new([ProviderProfile::bearer(
            "env://PROVIDER_ONE_KEY",
            "provider-one",
            "https://provider.example",
            vec![GatewayOperation::OpenAiResponses],
            vec![tenant_id],
            Arc::<str>::from("server-secret"),
        )])
        .unwrap()
    }

    fn id(output: &crate::admin::ResourceMutationOutput) -> Uuid {
        Uuid::parse_str(output.resource["id"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn model_metadata_is_bounded_and_selector_namespace_is_reserved() {
        let operations = vec!["openai_responses".to_owned()];
        assert!(
            validate_model_fields(
                "company-model",
                "provider-model",
                Some("description"),
                Some("standard"),
                &operations,
                &serde_json::Map::new(),
            )
            .is_ok()
        );
        assert!(
            validate_model_fields(
                "quality:gold",
                "provider-model",
                None,
                None,
                &operations,
                &serde_json::Map::new(),
            )
            .is_err()
        );
        let oversized = serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(
            json!({"price": "x".repeat(4097)}),
        )
        .unwrap();
        assert!(
            validate_model_fields(
                "company-model",
                "provider-model",
                None,
                None,
                &operations,
                &oversized,
            )
            .is_err()
        );
    }

    #[sqlx::test]
    async fn nine_resource_crud_is_tenant_safe_versioned_bounded_and_audited(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let actor_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO admin_accounts (id, tenant_id, display_name, role) VALUES ($1, $2, 'operator', 'operator')",
        )
        .bind(actor_id)
        .bind(tenant_id)
        .execute(&pool)
        .await
        .unwrap();
        let context = AdminContext {
            tenant_id,
            account_id: actor_id,
            session_id: Uuid::new_v4(),
            role: AdminRole::Operator,
        };
        let other_tenant = AdminContext {
            tenant_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            role: AdminRole::Admin,
        };
        let service = service(pool.clone());
        let source = IpAddr::from_str("127.0.0.1").unwrap();

        let user = service
            .create_resource(
                &context,
                ResourceKind::Users,
                json!({"display_name": "user"}),
                source,
                source,
                "create-user",
            )
            .await
            .unwrap();
        let user_id = id(&user);
        let second_user = service
            .create_resource(
                &context,
                ResourceKind::Users,
                json!({"display_name": "second user"}),
                source,
                source,
                "create-second-user",
            )
            .await
            .unwrap();
        let device = service
            .create_resource(
                &context,
                ResourceKind::Devices,
                json!({"display_name": "device"}),
                source,
                source,
                "create-device",
            )
            .await
            .unwrap();
        let device_id = id(&device);
        let client = service
            .create_resource(
                &context,
                ResourceKind::Clients,
                json!({"display_name": "client"}),
                source,
                source,
                "create-client",
            )
            .await
            .unwrap();
        let client_id = id(&client);
        let agent = service
            .create_resource(
                &context,
                ResourceKind::Agents,
                json!({"display_name": "agent"}),
                source,
                source,
                "create-agent",
            )
            .await
            .unwrap();
        let agent_id = id(&agent);
        let credential = service
            .create_resource(
                &context,
                ResourceKind::Credentials,
                json!({
                    "display_name": "client key",
                    "user_id": user_id,
                    "device_id": device_id,
                    "client_id": client_id,
                    "agent_id": agent_id
                }),
                source,
                source,
                "create-credential",
            )
            .await
            .unwrap();
        let credential_id = id(&credential);
        let plaintext = credential.secret.as_deref().unwrap();
        service.credentials.resolve(plaintext).await.unwrap();

        let provider = service
            .create_resource(
                &context,
                ResourceKind::Providers,
                json!({
                    "provider_key": "openai-primary",
                    "display_name": "OpenAI primary",
                    "provider_type": "openai",
                    "base_url": "https://api.openai.com",
                    "credential_ref": "env://OPENAI_PRIMARY_KEY",
                    "supported_operations": ["openai_responses"]
                }),
                source,
                source,
                "create-provider",
            )
            .await
            .unwrap();
        let provider_id = id(&provider);
        let model = service
            .create_resource(
                &context,
                ResourceKind::Models,
                json!({
                    "provider_id": provider_id,
                    "public_model_id": "company-default",
                    "provider_model_id": "provider-model",
                    "supported_operations": ["openai_responses"]
                }),
                source,
                source,
                "create-model",
            )
            .await
            .unwrap();
        let model_id = id(&model);
        let policy = service
            .create_resource(
                &context,
                ResourceKind::Policies,
                json!({
                    "policy_key": "default",
                    "revision": 1,
                    "credential_id": credential_id,
                    "configuration": {"models": ["company-default"]}
                }),
                source,
                source,
                "create-policy",
            )
            .await
            .unwrap();
        let policy_id = id(&policy);
        let quota = service
            .create_resource(
                &context,
                ResourceKind::Quotas,
                json!({
                    "quota_key": "model-default",
                    "revision": 1,
                    "model_id": model_id,
                    "configuration": {"requests": 100}
                }),
                source,
                source,
                "create-quota",
            )
            .await
            .unwrap();
        let quota_id = id(&quota);

        let first_page = service
            .list_resources(
                &context,
                ResourceKind::Users,
                ListQuery {
                    limit: Some(1),
                    after: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(first_page.items.len(), 1);
        let next_after = first_page.next_after.expect("second page was not exposed");
        let second_page = service
            .list_resources(
                &context,
                ResourceKind::Users,
                ListQuery {
                    limit: Some(1),
                    after: Some(next_after),
                },
            )
            .await
            .unwrap();
        assert_eq!(second_page.items.len(), 1);
        assert!(second_page.next_after.is_none());
        assert_ne!(first_page.items[0]["id"], second_page.items[0]["id"]);
        assert!(
            first_page.items[0]["id"] == second_user.resource["id"]
                || second_page.items[0]["id"] == second_user.resource["id"]
        );
        assert!(matches!(
            service
                .list_resources(
                    &context,
                    ResourceKind::Users,
                    ListQuery {
                        limit: Some(101),
                        after: None,
                    },
                )
                .await,
            Err(AdminError::InvalidRequest)
        ));
        assert!(matches!(
            service
                .get_resource(&other_tenant, ResourceKind::Users, user_id)
                .await,
            Err(AdminError::NotFound)
        ));

        service
            .update_resource(
                &context,
                ResourceTarget::new(ResourceKind::Credentials, credential_id),
                json!({"version": 1, "display_name": "renamed key"}),
                source,
                source,
                "update-credential",
            )
            .await
            .unwrap();
        let rotated = service
            .rotate_credential(
                &context,
                credential_id,
                RotateCredentialInput {
                    version: 2,
                    overlap_seconds: 60,
                    expires_at: None,
                },
                source,
                source,
                "rotate-credential",
            )
            .await
            .unwrap();
        let rotated_id = id(&rotated);
        service
            .delete_resource(
                &context,
                ResourceTarget::new(ResourceKind::Credentials, rotated_id),
                1,
                source,
                source,
                "revoke-credential",
            )
            .await
            .unwrap();
        service
            .update_resource(
                &context,
                ResourceTarget::new(ResourceKind::Providers, provider_id),
                json!({"version": 1, "display_name": "renamed provider"}),
                source,
                source,
                "update-provider",
            )
            .await
            .unwrap();
        service
            .update_resource(
                &context,
                ResourceTarget::new(ResourceKind::Models, model_id),
                json!({"version": 1, "provider_model_id": "provider-model-v2"}),
                source,
                source,
                "update-model",
            )
            .await
            .unwrap();
        service
            .update_resource(
                &context,
                ResourceTarget::new(ResourceKind::Policies, policy_id),
                json!({"version": 1, "configuration": {"models": ["company-default"]}}),
                source,
                source,
                "update-policy",
            )
            .await
            .unwrap();
        service
            .update_resource(
                &context,
                ResourceTarget::new(ResourceKind::Quotas, quota_id),
                json!({"version": 1, "configuration": {"requests": 200}}),
                source,
                source,
                "update-quota",
            )
            .await
            .unwrap();
        assert!(matches!(
            service
                .update_resource(
                    &context,
                    ResourceTarget::new(ResourceKind::Providers, provider_id),
                    json!({"version": 1, "display_name": "stale"}),
                    source,
                    source,
                    "stale-provider",
                )
                .await,
            Err(AdminError::Conflict)
        ));

        service
            .delete_resource(
                &context,
                ResourceTarget::new(ResourceKind::Users, user_id),
                1,
                source,
                source,
                "disable-user",
            )
            .await
            .unwrap();
        assert!(matches!(
            service.credentials.resolve(plaintext).await,
            Err(IdentityError::InvalidCredential)
        ));
        let archived = service
            .delete_resource(
                &context,
                ResourceTarget::new(ResourceKind::Models, model_id),
                2,
                source,
                source,
                "archive-model",
            )
            .await
            .unwrap();
        assert_eq!(archived.resource["lifecycle_state"], "archived");

        let audit_count: i64 = sqlx::query(
            "SELECT count(*) AS count FROM audit_logs WHERE tenant_id = $1 AND actor_admin_id = $2",
        )
        .bind(tenant_id)
        .bind(actor_id)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("count");
        assert_eq!(audit_count, 19);
    }

    #[sqlx::test]
    async fn registry_lifecycle_is_trusted_tenant_safe_and_cascading(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let actor_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO admin_accounts (id, tenant_id, display_name, role) VALUES ($1, $2, 'admin', 'admin')",
        )
        .bind(actor_id)
        .bind(tenant_id)
        .execute(&pool)
        .await
        .unwrap();
        let context = AdminContext {
            tenant_id,
            account_id: actor_id,
            session_id: Uuid::new_v4(),
            role: AdminRole::Admin,
        };
        let other_tenant = AdminContext {
            tenant_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            role: AdminRole::Admin,
        };
        let service = service(pool.clone()).with_provider_profiles(provider_profiles(tenant_id));
        let source = IpAddr::from_str("127.0.0.1").unwrap();

        let provider = service
            .create_resource(
                &context,
                ResourceKind::Providers,
                json!({
                    "provider_key": "provider-one",
                    "display_name": "Provider One",
                    "provider_type": "openai",
                    "base_url": "https://provider.example",
                    "credential_ref": "env://PROVIDER_ONE_KEY",
                    "supported_operations": ["openai_responses"]
                }),
                source,
                source,
                "create-provider",
            )
            .await
            .unwrap();
        let provider_id = id(&provider);
        let model = service
            .create_resource(
                &context,
                ResourceKind::Models,
                json!({
                    "provider_id": provider_id,
                    "public_model_id": "company-model",
                    "provider_model_id": "provider-model",
                    "display_name": "Company Model",
                    "supported_operations": ["openai_responses"],
                    "supports_streaming": true
                }),
                source,
                source,
                "create-model",
            )
            .await
            .unwrap();
        let model_id = id(&model);
        let alias = service
            .create_resource(
                &context,
                ResourceKind::ModelAliases,
                json!({
                    "operation": "openai_responses",
                    "quality_tier": "premium",
                    "model_id": model_id
                }),
                source,
                source,
                "create-alias",
            )
            .await
            .unwrap();
        let alias_id = id(&alias);

        assert!(matches!(
            service
                .set_registry_lifecycle(
                    &context,
                    ResourceTarget::new(ResourceKind::Models, model_id),
                    LifecycleInput {
                        version: 1,
                        state: "active".to_owned()
                    },
                    source,
                    source,
                    "activate-too-early",
                )
                .await,
            Err(AdminError::Conflict)
        ));
        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::Providers, provider_id),
                LifecycleInput {
                    version: 1,
                    state: "active".to_owned(),
                },
                source,
                source,
                "activate-provider",
            )
            .await
            .unwrap();
        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::Models, model_id),
                LifecycleInput {
                    version: 1,
                    state: "active".to_owned(),
                },
                source,
                source,
                "activate-model",
            )
            .await
            .unwrap();
        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::ModelAliases, alias_id),
                LifecycleInput {
                    version: 1,
                    state: "active".to_owned(),
                },
                source,
                source,
                "activate-alias",
            )
            .await
            .unwrap();
        assert!(matches!(
            service
                .set_registry_lifecycle(
                    &other_tenant,
                    ResourceTarget::new(ResourceKind::Providers, provider_id),
                    LifecycleInput {
                        version: 2,
                        state: "disabled".to_owned()
                    },
                    source,
                    source,
                    "cross-tenant-disable",
                )
                .await,
            Err(AdminError::NotFound)
        ));
        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::Providers, provider_id),
                LifecycleInput {
                    version: 2,
                    state: "disabled".to_owned(),
                },
                source,
                source,
                "disable-provider",
            )
            .await
            .unwrap();

        let states: (String, String, String) = sqlx::query_as(
            r#"
            SELECT p.lifecycle_state, m.lifecycle_state, a.lifecycle_state
            FROM providers p
            JOIN models m ON (m.tenant_id, m.provider_id) = (p.tenant_id, p.id)
            JOIN model_aliases a ON (a.tenant_id, a.model_id) = (m.tenant_id, m.id)
            WHERE p.tenant_id = $1 AND p.id = $2
            "#,
        )
        .bind(tenant_id)
        .bind(provider_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            states,
            (
                "disabled".to_owned(),
                "disabled".to_owned(),
                "disabled".to_owned()
            )
        );
        let cascade_audits: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_logs WHERE tenant_id = $1 AND action IN ('admin.model.disabled', 'admin.model_alias.disabled')",
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(cascade_audits, 2);

        let provider = service
            .update_resource(
                &context,
                ResourceTarget::new(ResourceKind::Providers, provider_id),
                json!({"version": 3, "display_name": "Provider One repaired"}),
                source,
                source,
                "repair-provider",
            )
            .await
            .unwrap();
        assert_eq!(provider.resource["version"], 4);
        let model = service
            .update_resource(
                &context,
                ResourceTarget::new(ResourceKind::Models, model_id),
                json!({"version": 3, "provider_model_id": "provider-model-v2"}),
                source,
                source,
                "repair-model",
            )
            .await
            .unwrap();
        assert_eq!(model.resource["version"], 4);

        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::Providers, provider_id),
                LifecycleInput {
                    version: 4,
                    state: "active".to_owned(),
                },
                source,
                source,
                "reactivate-provider",
            )
            .await
            .unwrap();
        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::Models, model_id),
                LifecycleInput {
                    version: 4,
                    state: "active".to_owned(),
                },
                source,
                source,
                "reactivate-model",
            )
            .await
            .unwrap();
        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::ModelAliases, alias_id),
                LifecycleInput {
                    version: 3,
                    state: "active".to_owned(),
                },
                source,
                source,
                "reactivate-alias",
            )
            .await
            .unwrap();
        let replacement = service
            .create_resource(
                &context,
                ResourceKind::ModelAliases,
                json!({"operation": "openai_responses", "quality_tier": "premium", "model_id": model_id}),
                source,
                source,
                "create-replacement-alias",
            )
            .await
            .unwrap();
        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::ModelAliases, id(&replacement)),
                LifecycleInput {
                    version: 1,
                    state: "active".to_owned(),
                },
                source,
                source,
                "swap-alias",
            )
            .await
            .unwrap();
        let active_aliases: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM model_aliases WHERE tenant_id = $1 AND operation = 'openai_responses' AND quality_tier = 'premium' AND lifecycle_state = 'active'",
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(active_aliases, 1);
    }

    #[sqlx::test]
    async fn audit_failure_rolls_back_the_resource_mutation(pool: PgPool) {
        let context = AdminContext {
            tenant_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            role: AdminRole::Operator,
        };
        let service = service(pool.clone());
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        assert!(matches!(
            service
                .create_resource(
                    &context,
                    ResourceKind::Users,
                    json!({"display_name": "must roll back"}),
                    source,
                    source,
                    "missing-actor",
                )
                .await,
            Err(AdminError::Database(_))
        ));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE tenant_id = $1")
            .bind(context.tenant_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
