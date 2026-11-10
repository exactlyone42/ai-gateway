use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::{Postgres, Row as _, Transaction};
use uuid::Uuid;

use crate::{
    model_registry::GatewayOperation,
    policy::validate_document as validate_policy_document,
    quota::{ADMISSION_LOCK_NAMESPACE, validate_document as validate_quota_document},
};

use super::{
    AdminContext, AdminError, AdminService, AuditEntry, append_audit, map_conflict, validate_name,
};

fn map_group_target_error(error: sqlx::Error) -> AdminError {
    if error
        .as_database_error()
        .and_then(|database| database.constraint())
        .is_some_and(|name| matches!(name, "policies_group_tenant_fk" | "quotas_group_tenant_fk"))
    {
        AdminError::NotFound
    } else {
        AdminError::Database(error)
    }
}

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
    ModelTaskProfiles,
    TaskClassifierConfigs,
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
            "model-task-profiles" => Ok(Self::ModelTaskProfiles),
            "task-classifier-configs" => Ok(Self::TaskClassifierConfigs),
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
            Self::ModelTaskProfiles => "model-task-profiles",
            Self::TaskClassifierConfigs => "task-classifier-configs",
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
            Self::ModelTaskProfiles => "model_task_profile",
            Self::TaskClassifierConfigs => "task_classifier_config",
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
        UNION ALL SELECT 'model-task-profiles', id, tenant_id, to_jsonb(model_task_profiles) FROM model_task_profiles
        UNION ALL SELECT 'task-classifier-configs', id, tenant_id, to_jsonb(task_classifier_configs) FROM task_classifier_configs
        UNION ALL SELECT 'policies', id, tenant_id, to_jsonb(policies) FROM policies
        UNION ALL SELECT 'quotas', id, tenant_id, to_jsonb(quotas) FROM quotas
    ) resources
    WHERE kind = $1 AND tenant_id = $2 AND id = $3
"#;

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<u16>,
    pub after: Option<Uuid>,
    pub email: Option<String>,
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
    email: Option<String>,
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
struct ModelTaskProfileCreate {
    model_id: Uuid,
    taxonomy_version: String,
    task_kind: String,
    suitability_score: i16,
    confidence: f64,
    source: String,
    source_ref: Option<String>,
    rationale: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskClassifierConfigCreate {
    taxonomy_version: String,
    classifier_model_id: Option<Uuid>,
    mode: String,
    minimum_confidence: f64,
    timeout_ms: i32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyCreate {
    policy_key: String,
    revision: i64,
    group_id: Option<Uuid>,
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
    group_id: Option<Uuid>,
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
    #[serde(default, deserialize_with = "deserialize_optional_email")]
    email: Option<Option<String>>,
}

fn deserialize_optional_email<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

fn validate_email(email: &str) -> Result<(), AdminError> {
    if email.trim().is_empty() || email.chars().count() > 320 {
        return Err(AdminError::InvalidRequest);
    }
    Ok(())
}

fn audit_resource(kind: ResourceKind, mut value: Value) -> Value {
    if kind == ResourceKind::Users
        && let Some(object) = value.as_object_mut()
    {
        object.remove("email");
    }
    value
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialPatch {
    version: i64,
    display_name: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    status: Option<String>,
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
struct ModelTaskProfilePatch {
    version: i64,
    suitability_score: Option<i16>,
    confidence: Option<f64>,
    source: Option<String>,
    source_ref: Option<String>,
    rationale: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskClassifierConfigPatch {
    version: i64,
    taxonomy_version: Option<String>,
    classifier_model_id: Option<Uuid>,
    mode: Option<String>,
    minimum_confidence: Option<f64>,
    timeout_ms: Option<i32>,
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
            ResourceKind::ModelTaskProfiles => {
                "SELECT to_jsonb(model_task_profiles) FROM model_task_profiles WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
            }
            ResourceKind::TaskClassifierConfigs => {
                "SELECT to_jsonb(task_classifier_configs) FROM task_classifier_configs WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
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
        if let Some(email) = query.email.as_deref() {
            if kind != ResourceKind::Users {
                return Err(AdminError::InvalidRequest);
            }
            validate_email(email)?;
        }
        let after = query.after.unwrap_or(Uuid::nil());
        let hidden_fields = kind
            .hidden_fields()
            .iter()
            .map(|field| (*field).to_owned())
            .collect::<Vec<_>>();
        let rows = sqlx::query_file!(
            "queries/admin/list_resources.sql",
            kind.slug(),
            context.tenant_id,
            after,
            i64::from(limit) + 1,
            &hidden_fields,
            query.email.as_deref()
        )
        .fetch_all(&self.pool)
        .await?;
        let has_more = rows.len() > usize::from(limit);
        let visible = rows.into_iter().take(usize::from(limit));
        let mut items = Vec::with_capacity(usize::from(limit));
        let mut last_id = None;
        for row in visible {
            last_id = Some(row.id);
            items.push(row.resource);
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
                if let Some(email) = input.email.as_deref() {
                    if kind != ResourceKind::Users {
                        return Err(AdminError::InvalidRequest);
                    }
                    validate_email(email)?;
                }
                if kind == ResourceKind::Users {
                    let value: Value = sqlx::query_file_scalar!(
                        "queries/admin/user_create.sql",
                        id,
                        context.tenant_id,
                        input.display_name,
                        input.email
                    )
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(map_conflict)?;
                    (value, None)
                } else {
                    let statement = match kind {
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
                    input.quality_tier.as_deref(),
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
            ResourceKind::ModelTaskProfiles => {
                let input: ModelTaskProfileCreate = decode(payload)?;
                if input.source == "migration" {
                    return Err(AdminError::InvalidRequest);
                }
                let value: Value = sqlx::query_scalar(
                    r#"
                    INSERT INTO model_task_profiles (
                        id, tenant_id, model_id, taxonomy_version, task_kind,
                        suitability_score, confidence, source, source_ref, rationale
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                    RETURNING to_jsonb(model_task_profiles)
                    "#,
                )
                .bind(id)
                .bind(context.tenant_id)
                .bind(input.model_id)
                .bind(input.taxonomy_version)
                .bind(input.task_kind)
                .bind(input.suitability_score)
                .bind(input.confidence)
                .bind(input.source)
                .bind(input.source_ref)
                .bind(input.rationale)
                .fetch_one(&mut *transaction)
                .await?;
                (value, None)
            }
            ResourceKind::TaskClassifierConfigs => {
                let input: TaskClassifierConfigCreate = decode(payload)?;
                if input.mode == "enforced" && context.role != super::AdminRole::Admin {
                    return Err(AdminError::Forbidden);
                }
                let value: Value = sqlx::query_scalar(
                    r#"
                    INSERT INTO task_classifier_configs (
                        id, tenant_id, taxonomy_version, classifier_model_id,
                        mode, minimum_confidence, timeout_ms
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                    RETURNING to_jsonb(task_classifier_configs)
                    "#,
                )
                .bind(id)
                .bind(context.tenant_id)
                .bind(input.taxonomy_version)
                .bind(input.classifier_model_id)
                .bind(input.mode)
                .bind(input.minimum_confidence)
                .bind(input.timeout_ms)
                .fetch_one(&mut *transaction)
                .await?;
                (value, None)
            }
            ResourceKind::Policies => {
                let input: PolicyCreate = decode(payload)?;
                validate_name(&input.policy_key)?;
                if input.revision <= 0
                    || input.policy_key.len() > 200
                    || [
                        input.group_id,
                        input.user_id,
                        input.device_id,
                        input.client_id,
                        input.agent_id,
                        input.credential_id,
                    ]
                    .into_iter()
                    .flatten()
                    .count()
                        > 1
                {
                    return Err(AdminError::InvalidRequest);
                }
                let configuration = Value::Object(input.configuration);
                validate_policy_document(&configuration).map_err(|_| AdminError::InvalidRequest)?;
                let value: Value = sqlx::query_file_scalar!(
                    "queries/admin/policy_create.sql",
                    id,
                    context.tenant_id,
                    input.policy_key,
                    input.revision,
                    input.user_id,
                    input.device_id,
                    input.client_id,
                    input.agent_id,
                    input.credential_id,
                    configuration,
                    input.group_id
                )
                .fetch_one(&mut *transaction)
                .await
                .map_err(map_group_target_error)?;
                (value, None)
            }
            ResourceKind::Quotas => {
                let input: QuotaCreate = decode(payload)?;
                if input.quota_key.trim() != input.quota_key
                    || input.quota_key.is_empty()
                    || input.quota_key.len() > 200
                    || input.revision <= 0
                    || [
                        input.group_id,
                        input.user_id,
                        input.device_id,
                        input.client_id,
                        input.agent_id,
                        input.credential_id,
                        input.model_id,
                        input.provider_id,
                    ]
                    .into_iter()
                    .flatten()
                    .count()
                        > 1
                {
                    return Err(AdminError::InvalidRequest);
                }
                let configuration = Value::Object(input.configuration);
                validate_quota_document(&configuration).map_err(|_| AdminError::InvalidRequest)?;
                let value: Value = sqlx::query_file_scalar!(
                    "queries/admin/quota_create.sql",
                    id,
                    context.tenant_id,
                    input.quota_key,
                    input.revision,
                    input.user_id,
                    input.device_id,
                    input.client_id,
                    input.agent_id,
                    input.credential_id,
                    input.model_id,
                    input.provider_id,
                    configuration,
                    input.group_id
                )
                .fetch_one(&mut *transaction)
                .await
                .map_err(map_group_target_error)?;
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
                after: Some(audit_resource(kind, resource.clone())),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        if kind == ResourceKind::Policies
            && let Some(policies) = &self.policies
        {
            policies.clear_tenant_cache(context.tenant_id);
        }
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
                if input.display_name.is_none() && input.status.is_none() && input.email.is_none() {
                    return Err(AdminError::InvalidRequest);
                }
                if input.email.is_some() && kind != ResourceKind::Users {
                    return Err(AdminError::InvalidRequest);
                }
                if let Some(Some(email)) = input.email.as_ref() {
                    validate_email(email)?;
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
                let resource: Value = if kind == ResourceKind::Users {
                    sqlx::query_file_scalar!(
                        "queries/admin/user_update.sql",
                        context.tenant_id,
                        id,
                        input.version,
                        input.display_name.as_deref(),
                        input.status.as_deref(),
                        input.email.is_some(),
                        input.email.as_ref().and_then(|value| value.as_deref())
                    )
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(map_conflict)?
                    .ok_or(AdminError::Conflict)?
                } else {
                    let statement = match kind {
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
                    let query = sqlx::query_scalar(statement)
                        .bind(context.tenant_id)
                        .bind(id)
                        .bind(input.version)
                        .bind(input.display_name.as_deref())
                        .bind(input.status.as_deref());
                    query
                        .fetch_optional(&mut *transaction)
                        .await
                        .map_err(map_conflict)?
                        .ok_or(AdminError::Conflict)?
                };
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
                if input.display_name.is_none()
                    && input.expires_at.is_none()
                    && input.status.is_none()
                {
                    return Err(AdminError::InvalidRequest);
                }
                if input
                    .status
                    .as_deref()
                    .is_some_and(|status| !matches!(status, "active" | "disabled"))
                {
                    return Err(AdminError::InvalidRequest);
                }
                if let Some(name) = input.display_name.as_deref() {
                    validate_name(name)?;
                }
                sqlx::query_file_scalar!(
                    "queries/admin/credential_update.sql",
                    context.tenant_id,
                    id,
                    input.version,
                    input.display_name,
                    input.expires_at,
                    input.status
                )
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
                      AND lifecycle_state IN ('draft', 'disabled', 'active')
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
                let configuration = Value::Object(input.configuration);
                if kind == ResourceKind::Policies {
                    validate_policy_document(&configuration)
                        .map_err(|_| AdminError::InvalidRequest)?;
                } else {
                    validate_quota_document(&configuration)
                        .map_err(|_| AdminError::InvalidRequest)?;
                }
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
                    .bind(configuration)
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
            ResourceKind::ModelTaskProfiles => {
                let input: ModelTaskProfilePatch = decode(payload)?;
                if input.source.as_deref() == Some("migration") {
                    return Err(AdminError::InvalidRequest);
                }
                sqlx::query_scalar(
                    r#"
                    UPDATE model_task_profiles SET
                        suitability_score = COALESCE($4, suitability_score),
                        confidence = COALESCE($5, confidence),
                        source = COALESCE($6, source),
                        source_ref = COALESCE($7, source_ref),
                        rationale = COALESCE($8, rationale),
                        version = version + 1, updated_at = now()
                    WHERE tenant_id = $1 AND id = $2 AND version = $3
                      AND lifecycle_state IN ('draft', 'disabled')
                    RETURNING to_jsonb(model_task_profiles)
                    "#,
                )
                .bind(context.tenant_id)
                .bind(id)
                .bind(input.version)
                .bind(input.suitability_score)
                .bind(input.confidence)
                .bind(input.source)
                .bind(input.source_ref)
                .bind(input.rationale)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or(AdminError::Conflict)?
            }
            ResourceKind::TaskClassifierConfigs => {
                let input: TaskClassifierConfigPatch = decode(payload)?;
                if input.mode.as_deref() == Some("enforced")
                    && context.role != super::AdminRole::Admin
                {
                    return Err(AdminError::Forbidden);
                }
                sqlx::query_scalar(
                    r#"
                    UPDATE task_classifier_configs SET
                        taxonomy_version = COALESCE($4, taxonomy_version),
                        classifier_model_id = COALESCE($5, classifier_model_id),
                        mode = COALESCE($6, mode),
                        minimum_confidence = COALESCE($7, minimum_confidence),
                        timeout_ms = COALESCE($8, timeout_ms),
                        version = version + 1, updated_at = now()
                    WHERE tenant_id = $1 AND id = $2 AND version = $3
                    RETURNING to_jsonb(task_classifier_configs)
                    "#,
                )
                .bind(context.tenant_id)
                .bind(id)
                .bind(input.version)
                .bind(input.taxonomy_version)
                .bind(input.classifier_model_id)
                .bind(input.mode)
                .bind(input.minimum_confidence)
                .bind(input.timeout_ms)
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
                before: Some(audit_resource(kind, before)),
                after: Some(audit_resource(kind, resource.clone())),
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
        if kind == ResourceKind::Policies
            && let Some(policies) = &self.policies
        {
            policies.clear_tenant_cache(context.tenant_id);
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
            | ResourceKind::ModelTaskProfiles
            | ResourceKind::Policies
            | ResourceKind::Quotas => {
                return self
                    .archive_draft(context, target, version, source_ip, peer_ip, request_id)
                    .await;
            }
            ResourceKind::TaskClassifierConfigs => {
                json!({"version": version, "mode": "disabled"})
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
            ResourceKind::Providers
                | ResourceKind::Models
                | ResourceKind::ModelAliases
                | ResourceKind::ModelTaskProfiles
                | ResourceKind::Policies
                | ResourceKind::Quotas
        ) {
            return Err(AdminError::InvalidRequest);
        }

        let mut transaction = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, $2))")
            .bind(context.tenant_id)
            .bind(if kind == ResourceKind::Quotas {
                ADMISSION_LOCK_NAMESPACE
            } else {
                0
            })
            .execute(&mut *transaction)
            .await?;
        let before = self
            .lock_resource(&mut transaction, context, kind, id)
            .await?;
        let cascaded_before = if input.state == "disabled" {
            load_registry_cascade(&mut transaction, context.tenant_id, kind, id).await?
        } else if kind == ResourceKind::ModelAliases {
            load_alias_replacement(&mut transaction, context.tenant_id, id).await?
        } else if kind == ResourceKind::Policies {
            load_policy_replacement(&mut transaction, context.tenant_id, id).await?
        } else if kind == ResourceKind::Quotas {
            load_quota_replacement(&mut transaction, context.tenant_id, id).await?
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
                    before["provider_type"].as_str().unwrap_or_default(),
                    before["base_url"].as_str().unwrap_or_default(),
                    &operations,
                )
            });
            if !trusted {
                return Err(AdminError::InvalidRequest);
            }
        }
        if kind == ResourceKind::Policies && input.state == "active" {
            validate_policy_document(&before["configuration"])
                .map_err(|_| AdminError::InvalidRequest)?;
            sqlx::query_file!("queries/admin/policy_activate.sql", context.tenant_id, id)
                .execute(&mut *transaction)
                .await?;
        }
        if kind == ResourceKind::Quotas && input.state == "active" {
            validate_quota_document(&before["configuration"])
                .map_err(|_| AdminError::InvalidRequest)?;
            sqlx::query_file!("queries/admin/quota_activate.sql", context.tenant_id, id)
                .execute(&mut *transaction)
                .await?;
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
                  AND provider_target_operation(p.provider_type, a.operation)
                      = ANY(p.supported_operations)
                  AND provider_target_operation(p.provider_type, a.operation)
                      = ANY(m.supported_operations)
                RETURNING to_jsonb(a)
                "#
            }
            (ResourceKind::ModelAliases, "disabled") => {
                "UPDATE model_aliases SET lifecycle_state = 'disabled', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state = 'active' RETURNING to_jsonb(model_aliases)"
            }
            (ResourceKind::ModelTaskProfiles, "active") => {
                r#"
                UPDATE model_task_profiles profile SET lifecycle_state = 'active',
                    version = profile.version + 1, updated_at = now()
                FROM models model
                WHERE (model.tenant_id, model.id) = (profile.tenant_id, profile.model_id)
                  AND profile.tenant_id = $1 AND profile.id = $2 AND profile.version = $3
                  AND profile.lifecycle_state IN ('draft', 'disabled')
                  AND profile.source <> 'migration' AND model.lifecycle_state = 'active'
                RETURNING to_jsonb(profile)
                "#
            }
            (ResourceKind::ModelTaskProfiles, "disabled") => {
                "UPDATE model_task_profiles SET lifecycle_state = 'disabled', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state = 'active' RETURNING to_jsonb(model_task_profiles)"
            }
            (ResourceKind::Policies, "active") => {
                "UPDATE policies SET lifecycle_state = 'active', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state IN ('draft', 'disabled') RETURNING to_jsonb(policies)"
            }
            (ResourceKind::Policies, "disabled") => {
                "UPDATE policies SET lifecycle_state = 'disabled', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state = 'active' RETURNING to_jsonb(policies)"
            }
            (ResourceKind::Quotas, "active") => {
                "UPDATE quotas SET lifecycle_state = 'active', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state IN ('draft', 'disabled') RETURNING to_jsonb(quotas)"
            }
            (ResourceKind::Quotas, "disabled") => {
                "UPDATE quotas SET lifecycle_state = 'disabled', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state = 'active' RETURNING to_jsonb(quotas)"
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
        if kind == ResourceKind::Policies
            && let Some(policies) = &self.policies
        {
            policies.clear_tenant_cache(context.tenant_id);
        }
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
                    fingerprint, user_id, device_id, client_id, agent_id, expires_at,
                    rotated_from_id, rotation_root_id
                )
                SELECT $5, $1, "type", display_name, $6, $7, $8, $9,
                       user_id, device_id, client_id, agent_id,
                       COALESCE($10, expires_at), $2, COALESCE(rotation_root_id, id)
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
            ResourceKind::ModelTaskProfiles => {
                "UPDATE model_task_profiles SET lifecycle_state = 'archived', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state IN ('draft', 'disabled') RETURNING to_jsonb(model_task_profiles)"
            }
            ResourceKind::Policies => {
                "UPDATE policies SET lifecycle_state = 'archived', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state IN ('draft', 'disabled') RETURNING to_jsonb(policies)"
            }
            ResourceKind::Quotas => {
                "UPDATE quotas SET lifecycle_state = 'archived', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND lifecycle_state IN ('draft', 'disabled') RETURNING to_jsonb(quotas)"
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
        if kind == ResourceKind::Policies
            && let Some(policies) = &self.policies
        {
            policies.clear_tenant_cache(context.tenant_id);
        }
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
    quality_tier: Option<&str>,
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
        || quality_tier.is_some_and(|value| !is_quality_tier(value))
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
        || input
            .quality_tier
            .as_deref()
            .is_some_and(|value| !is_quality_tier(value))
    {
        return Err(AdminError::InvalidRequest);
    }
    if let Some(operations) = input.supported_operations.as_deref() {
        validate_operations(operations)?;
    }
    Ok(())
}

fn is_quality_tier(value: &str) -> bool {
    matches!(value, "economy" | "standard" | "advanced" | "premium")
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

async fn load_policy_replacement(
    transaction: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Vec<Value>, AdminError> {
    let rows: Value =
        sqlx::query_file_scalar!("queries/admin/policy_replacement.sql", tenant_id, id)
            .fetch_one(&mut **transaction)
            .await?;
    Ok(rows.as_array().cloned().unwrap_or_default())
}

async fn load_quota_replacement(
    transaction: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Vec<Value>, AdminError> {
    let rows: Value =
        sqlx::query_file_scalar!("queries/admin/quota_replacement.sql", tenant_id, id)
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
               CASE entries.value->>'target_type'
                   WHEN 'policy' THEN (SELECT to_jsonb(p) FROM policies p
                       WHERE p.tenant_id = $3 AND p.id = (entries.value->>'id')::uuid)
                   WHEN 'quota' THEN (SELECT to_jsonb(q) FROM quotas q
                       WHERE q.tenant_id = $3 AND q.id = (entries.value->>'id')::uuid)
                   WHEN 'model' THEN (SELECT to_jsonb(m) FROM models m
                       WHERE m.tenant_id = $3 AND m.id = (entries.value->>'id')::uuid)
                   WHEN 'model_alias' THEN (SELECT to_jsonb(a) FROM model_aliases a
                       WHERE a.tenant_id = $3 AND a.id = (entries.value->>'id')::uuid)
               END,
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

    use serde_json::{Value, json};
    use sqlx::{PgPool, Row as _};
    use uuid::Uuid;

    use super::{
        LifecycleInput, ListQuery, ResourceKind, ResourceTarget, RotateCredentialInput,
        validate_model_fields,
    };
    use crate::{
        admin::{AdminContext, AdminError, AdminRole, AdminService, AdminServiceConfig},
        identity::{CredentialService, IdentityError},
        model_registry::{
            CapabilityRequirements, GatewayOperation, ModelRegistry, ModelSelector,
            ProviderDialect, ProviderProfile, ProviderProfiles,
        },
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
            ProviderDialect::OpenAi,
            vec![GatewayOperation::OpenAiResponses],
            vec![tenant_id],
            Arc::<str>::from("server-secret"),
        )])
        .unwrap()
    }

    fn id(output: &crate::admin::ResourceMutationOutput) -> Uuid {
        Uuid::parse_str(output.resource["id"].as_str().unwrap()).unwrap()
    }

    #[sqlx::test]
    async fn cross_provider_quality_alias_activates_through_the_admin_lifecycle(pool: PgPool) {
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
        let profiles = provider_profiles(tenant_id);
        let service = service(pool.clone()).with_provider_profiles(profiles.clone());
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let provider = service
            .create_resource(
                &context,
                ResourceKind::Providers,
                json!({
                    "provider_key": "provider-one",
                    "display_name": "OpenAI target",
                    "provider_type": "openai",
                    "base_url": "https://provider.example",
                    "credential_ref": "env://PROVIDER_ONE_KEY",
                    "supported_operations": ["openai_responses"]
                }),
                source,
                source,
                "create-cross-provider",
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
                    "public_model_id": "cross-model",
                    "provider_model_id": "provider-model",
                    "display_name": "Cross model",
                    "supported_operations": ["openai_responses"]
                }),
                source,
                source,
                "create-cross-model",
            )
            .await
            .unwrap();
        let model_id = id(&model);
        let alias = service
            .create_resource(
                &context,
                ResourceKind::ModelAliases,
                json!({
                    "operation": "anthropic_messages",
                    "quality_tier": "premium",
                    "model_id": model_id
                }),
                source,
                source,
                "create-cross-alias",
            )
            .await
            .unwrap();
        let alias_id = id(&alias);
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
                "activate-cross-provider",
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
                "activate-cross-model",
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
                "activate-cross-alias",
            )
            .await
            .unwrap();

        let active: bool = sqlx::query_scalar(
            "SELECT lifecycle_state = 'active' FROM model_aliases WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(alias_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(active);
        let candidates = ModelRegistry::new(pool, profiles, Vec::new())
            .candidates(
                tenant_id,
                false,
                GatewayOperation::AnthropicMessages,
                ModelSelector::parse("quality:premium").unwrap(),
                CapabilityRequirements::default(),
            )
            .await
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].supports_target_operation(GatewayOperation::OpenAiResponses));
    }

    #[sqlx::test]
    async fn provider_target_operations_cover_every_cross_protocol_kind(pool: PgPool) {
        let mappings: Vec<String> = sqlx::query_scalar(
            r#"
            SELECT provider_target_operation(provider_type, source_operation)
            FROM (VALUES
                (1, 'openai', 'anthropic_messages'),
                (2, 'xai', 'anthropic_messages'),
                (3, 'local', 'anthropic_messages'),
                (4, 'anthropic', 'openai_responses'),
                (5, 'gemini', 'openai_responses')
            ) AS cases(ordinal, provider_type, source_operation)
            ORDER BY ordinal
            "#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            mappings,
            [
                "openai_responses",
                "openai_responses",
                "openai_responses",
                "anthropic_messages",
                "gemini_generate_content",
            ]
        );
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
                Some("standard"),
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
                None,
            )
            .is_err()
        );
        assert!(
            validate_model_fields(
                "company-model",
                "provider-model",
                None,
                None,
                &operations,
                &serde_json::Map::new(),
                Some("best"),
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
                None,
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
                    "configuration": {"constraints": {"allowed_models": ["company-default"]}}
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
                    "configuration": {"mode": "hard", "limits": {"requests_per_minute": 100}}
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
                    email: None,
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
                    email: None,
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
                        email: None,
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
                json!({"version": 1, "configuration": {"constraints": {"allowed_models": ["company-default"]}}}),
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
                json!({"version": 1, "configuration": {"mode": "hard", "limits": {"requests_per_minute": 200}}}),
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

    #[sqlx::test]
    async fn policy_activation_replaces_one_scope_atomically_and_honors_cas(pool: PgPool) {
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
        let service = service(pool.clone());
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let first = service
            .create_resource(
                &context,
                ResourceKind::Policies,
                json!({
                    "policy_key": "global-default",
                    "revision": 1,
                    "configuration": {"defaults": {"quality": "standard"}}
                }),
                source,
                source,
                "create-policy-one",
            )
            .await
            .unwrap();
        let second = service
            .create_resource(
                &context,
                ResourceKind::Policies,
                json!({
                    "policy_key": "global-default",
                    "revision": 2,
                    "configuration": {"defaults": {"quality": "advanced"}}
                }),
                source,
                source,
                "create-policy-two",
            )
            .await
            .unwrap();
        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::Policies, id(&first)),
                LifecycleInput {
                    version: 1,
                    state: "active".to_owned(),
                },
                source,
                source,
                "activate-policy-one",
            )
            .await
            .unwrap();
        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::Policies, id(&second)),
                LifecycleInput {
                    version: 1,
                    state: "active".to_owned(),
                },
                source,
                source,
                "activate-policy-two",
            )
            .await
            .unwrap();

        let stale = service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::Policies, id(&first)),
                LifecycleInput {
                    version: 2,
                    state: "active".to_owned(),
                },
                source,
                source,
                "stale-reactivation",
            )
            .await;
        assert!(matches!(stale, Err(AdminError::Conflict)));
        let state: Value = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'first', (SELECT lifecycle_state FROM policies WHERE id = $1),
                'second', (SELECT lifecycle_state FROM policies WHERE id = $2),
                'active_count', (SELECT count(*) FROM policies
                    WHERE tenant_id = $3 AND lifecycle_state = 'active'),
                'replacement_audits', (SELECT count(*) FROM audit_logs
                    WHERE tenant_id = $3 AND action = 'admin.policy.disabled'),
                'replacement_audit_exact', (SELECT after_state = to_jsonb(p)
                    FROM audit_logs a JOIN policies p
                      ON (p.tenant_id, p.id) = (a.tenant_id, a.target_id)
                    WHERE a.tenant_id = $3 AND a.action = 'admin.policy.disabled'
                    LIMIT 1)
            )
            "#,
        )
        .bind(id(&first))
        .bind(id(&second))
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state["first"], "disabled");
        assert_eq!(state["second"], "active");
        assert_eq!(state["active_count"], 1);
        assert_eq!(state["replacement_audits"], 1);
        assert_eq!(state["replacement_audit_exact"], true);
    }

    #[sqlx::test]
    async fn user_email_creation_update_clear_and_duplicate_are_checked(pool: PgPool) {
        sqlx::raw_sql(include_str!("../../tests/group_admin_actor.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let context = AdminContext {
            tenant_id: Uuid::from_u128(0x100),
            account_id: Uuid::from_u128(0x99),
            session_id: Uuid::new_v4(),
            role: AdminRole::Admin,
        };
        let service = service(pool.clone());
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let first = service
            .create_resource(
                &context,
                ResourceKind::Users,
                json!({"display_name":"first","email":"Cash@example.com"}),
                source,
                source,
                "email-create",
            )
            .await
            .unwrap();
        assert_eq!(first.resource["email"], "Cash@example.com");
        let search = || ListQuery {
            limit: Some(10),
            after: None,
            email: Some("cash@EXAMPLE.com".to_owned()),
        };
        let found = service
            .list_resources(&context, ResourceKind::Users, search())
            .await
            .unwrap();
        assert_eq!(found.items.len(), 1);
        assert_eq!(found.items[0]["id"], id(&first).to_string());
        let other = AdminContext {
            tenant_id: Uuid::from_u128(0x200),
            ..context.clone()
        };
        assert!(
            service
                .list_resources(&other, ResourceKind::Users, search())
                .await
                .unwrap()
                .items
                .is_empty()
        );
        let without_tenant_filter = include_str!("../../queries/admin/list_resources.sql")
            .replace("tenant_id = $2", "$2::uuid IS NOT NULL");
        let leaked = sqlx::query(&without_tenant_filter)
            .bind("users")
            .bind(other.tenant_id)
            .bind(Uuid::nil())
            .bind(11_i64)
            .bind(Vec::<String>::new())
            .bind("cash@EXAMPLE.com")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(leaked.len(), 1);
        assert!(matches!(
            service
                .list_resources(&context, ResourceKind::Devices, search())
                .await,
            Err(AdminError::InvalidRequest)
        ));
        let duplicate = service
            .create_resource(
                &context,
                ResourceKind::Users,
                json!({"display_name":"second","email":"cash@example.com"}),
                source,
                source,
                "email-duplicate",
            )
            .await;
        assert!(matches!(duplicate, Err(AdminError::Conflict)));
        let invalid = service
            .create_resource(
                &context,
                ResourceKind::Devices,
                json!({"display_name":"device","email":"x@example.com"}),
                source,
                source,
                "email-device",
            )
            .await;
        assert!(matches!(invalid, Err(AdminError::InvalidRequest)));
        let unchanged = service
            .update_resource(
                &context,
                ResourceTarget::new(ResourceKind::Users, id(&first)),
                json!({"version":1,"display_name":"changed"}),
                source,
                source,
                "email-keep",
            )
            .await
            .unwrap();
        assert_eq!(unchanged.resource["email"], "Cash@example.com");
        let cleared = service
            .update_resource(
                &context,
                ResourceTarget::new(ResourceKind::Users, id(&first)),
                json!({"version":2,"email":null}),
                source,
                source,
                "email-clear",
            )
            .await
            .unwrap();
        assert!(cleared.resource["email"].is_null());
        let reused = service
            .create_resource(
                &context,
                ResourceKind::Users,
                json!({"display_name":"second","email":"cash@example.com"}),
                source,
                source,
                "email-reuse",
            )
            .await;
        assert!(reused.is_ok());
        let leaked: i64 =
            sqlx::query_file_scalar!("tests/user_email_audit_count.sql", context.tenant_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(leaked, 0);
    }

    #[sqlx::test]
    async fn credential_status_update_invalidates_cached_key(pool: PgPool) {
        sqlx::raw_sql(include_str!("../../tests/group_admin_actor.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let context = AdminContext {
            tenant_id: Uuid::from_u128(0x100),
            account_id: Uuid::from_u128(0x99),
            session_id: Uuid::new_v4(),
            role: AdminRole::Admin,
        };
        let other_process = service(pool.clone());
        let service = service(pool.clone());
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let user = service
            .create_resource(
                &context,
                ResourceKind::Users,
                json!({"display_name":"user"}),
                source,
                source,
                "create-user",
            )
            .await
            .unwrap();
        let credential = service
            .create_resource(
                &context,
                ResourceKind::Credentials,
                json!({"display_name":"key","user_id":id(&user)}),
                source,
                source,
                "create-key",
            )
            .await
            .unwrap();
        let secret = credential.secret.as_ref().unwrap();
        assert!(service.credentials.resolve(secret).await.is_ok());
        assert!(other_process.credentials.resolve(secret).await.is_ok());
        let disabled = service
            .update_resource(
                &context,
                ResourceTarget::new(ResourceKind::Credentials, id(&credential)),
                json!({"version":1,"status":"disabled"}),
                source,
                source,
                "disable-key",
            )
            .await
            .unwrap();
        assert_eq!(disabled.resource["status"], "disabled");
        assert!(service.credentials.resolve(secret).await.is_err());
        assert!(other_process.credentials.resolve(secret).await.is_err());
        let stale = service
            .update_resource(
                &context,
                ResourceTarget::new(ResourceKind::Credentials, id(&credential)),
                json!({"version":1,"status":"active"}),
                source,
                source,
                "stale-key",
            )
            .await;
        assert!(matches!(stale, Err(AdminError::Conflict)));
        let enabled = service
            .update_resource(
                &context,
                ResourceTarget::new(ResourceKind::Credentials, id(&credential)),
                json!({"version":2,"status":"active"}),
                source,
                source,
                "enable-key",
            )
            .await
            .unwrap();
        assert_eq!(enabled.resource["status"], "active");
        assert!(service.credentials.resolve(secret).await.is_ok());
        sqlx::query_file!(
            "tests/credential_disable_without_invalidation.sql",
            context.tenant_id,
            id(&credential)
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(service.credentials.resolve(secret).await.is_err());
    }

    #[sqlx::test]
    async fn disabling_user_revokes_credential_even_after_user_is_reenabled(pool: PgPool) {
        sqlx::raw_sql(include_str!("../../tests/group_admin_actor.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let context = AdminContext {
            tenant_id: Uuid::from_u128(0x100),
            account_id: Uuid::from_u128(0x99),
            session_id: Uuid::new_v4(),
            role: AdminRole::Admin,
        };
        let service = service(pool);
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let user = service
            .create_resource(
                &context,
                ResourceKind::Users,
                json!({"display_name":"user"}),
                source,
                source,
                "user",
            )
            .await
            .unwrap();
        let credential = service
            .create_resource(
                &context,
                ResourceKind::Credentials,
                json!({"display_name":"key", "user_id":id(&user)}),
                source,
                source,
                "key",
            )
            .await
            .unwrap();
        let secret = credential.secret.as_ref().unwrap();
        assert!(service.credentials.resolve(secret).await.is_ok());
        for (version, status) in [(1, "disabled"), (2, "active")] {
            service
                .update_resource(
                    &context,
                    ResourceTarget::new(ResourceKind::Users, id(&user)),
                    json!({"version":version,"status":status}),
                    source,
                    source,
                    "user-status",
                )
                .await
                .unwrap();
        }
        assert!(service.credentials.resolve(secret).await.is_err());
        let saved = service
            .get_resource(&context, ResourceKind::Credentials, id(&credential))
            .await
            .unwrap();
        assert_eq!(saved["status"], "revoked");
    }

    #[sqlx::test]
    async fn group_membership_rejects_other_tenants_and_tracks_changes(pool: PgPool) {
        sqlx::raw_sql(include_str!("../../tests/group_selection_fixture.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../../tests/group_admin_actor.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let context = AdminContext {
            tenant_id: Uuid::from_u128(0x100),
            account_id: Uuid::from_u128(0x99),
            session_id: Uuid::new_v4(),
            role: AdminRole::Admin,
        };
        let service = service(pool);
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let user = Uuid::from_u128(1);
        let group = Uuid::from_u128(0x10);
        let foreign = Uuid::from_u128(0x20);
        let query = || serde_json::from_value(json!({})).unwrap();
        assert_eq!(
            service
                .list_group_members(&context, group, query())
                .await
                .unwrap()
                .items
                .len(),
            1
        );
        assert!(matches!(
            service
                .change_group_member(
                    &context,
                    (group, user),
                    true,
                    None,
                    (source, source),
                    "duplicate"
                )
                .await,
            Err(AdminError::Conflict)
        ));
        assert!(matches!(
            service
                .change_group_member(
                    &context,
                    (foreign, user),
                    true,
                    None,
                    (source, source),
                    "foreign"
                )
                .await,
            Err(AdminError::NotFound)
        ));
        service
            .change_group_member(
                &context,
                (group, user),
                false,
                None,
                (source, source),
                "remove",
            )
            .await
            .unwrap();
        assert!(
            service
                .list_group_members(&context, group, query())
                .await
                .unwrap()
                .items
                .is_empty()
        );
        assert!(matches!(
            service
                .change_group_member(
                    &context,
                    (group, user),
                    false,
                    None,
                    (source, source),
                    "duplicate-remove"
                )
                .await,
            Err(AdminError::NotFound)
        ));
        service
            .change_group_member(&context, (group, user), true, None, (source, source), "add")
            .await
            .unwrap();
        assert_eq!(
            service
                .list_group_members(&context, group, query())
                .await
                .unwrap()
                .items
                .len(),
            1
        );
    }

    #[sqlx::test]
    async fn group_member_change_rolls_back_when_audit_fails(pool: PgPool) {
        sqlx::raw_sql(include_str!("../../tests/group_selection_fixture.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let context = AdminContext {
            tenant_id: Uuid::from_u128(0x100),
            account_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            role: AdminRole::Admin,
        };
        let service = service(pool);
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let group = Uuid::from_u128(0x10);
        let user = Uuid::from_u128(1);
        let result = service
            .change_group_member(
                &context,
                (group, user),
                false,
                None,
                (source, source),
                "missing-audit-actor",
            )
            .await;
        assert!(result.is_err());
        let page = service
            .list_group_members(&context, group, serde_json::from_value(json!({})).unwrap())
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0]["user_id"], user.to_string());
    }

    #[sqlx::test]
    async fn membership_changes_invalidate_cached_group_policies(pool: PgPool) {
        sqlx::raw_sql(include_str!("../../tests/group_selection_fixture.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../../tests/group_admin_actor.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let context = AdminContext {
            tenant_id: Uuid::from_u128(0x100),
            account_id: Uuid::from_u128(0x99),
            session_id: Uuid::new_v4(),
            role: AdminRole::Admin,
        };
        let user = Uuid::from_u128(1);
        let group = Uuid::from_u128(0x10);
        let principal = crate::identity::PrincipalContext {
            tenant_id: context.tenant_id,
            credential_id: Uuid::new_v4(),
            credential_name: "test".into(),
            user_id: Some(user),
            user_name: None,
            device_id: None,
            device_name: None,
            client_id: None,
            client_name: None,
            agent_id: None,
            agent_name: None,
        };
        let engine =
            crate::policy::PolicyEngine::with_cache(pool.clone(), 8, Duration::from_secs(60));
        let other_engine =
            crate::policy::PolicyEngine::with_cache(pool.clone(), 8, Duration::from_secs(60));
        let no_invalidation = service(pool);
        let service = service(no_invalidation.pool.clone()).with_policy_engine(engine.clone());
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let selector = ModelSelector::parse("company-default").unwrap();
        let headers = axum::http::HeaderMap::new();
        let evaluate = || engine.evaluate(&principal, &headers, &selector, chrono::Utc::now());
        let evaluate_other =
            || other_engine.evaluate(&principal, &headers, &selector, chrono::Utc::now());
        assert_eq!(
            evaluate().await.unwrap().decision()["applied_policies"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            evaluate_other().await.unwrap().decision()["applied_policies"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        no_invalidation
            .change_group_member(
                &context,
                (group, user),
                false,
                None,
                (source, source),
                "no-invalidation",
            )
            .await
            .unwrap();
        assert_eq!(
            evaluate().await.unwrap().decision()["applied_policies"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            evaluate_other().await.unwrap().decision()["applied_policies"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        service
            .change_group_member(&context, (group, user), true, None, (source, source), "add")
            .await
            .unwrap();
        assert_eq!(
            evaluate().await.unwrap().decision()["applied_policies"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        service
            .change_group_member(
                &context,
                (group, user),
                false,
                None,
                (source, source),
                "remove",
            )
            .await
            .unwrap();
        let result = evaluate().await.unwrap();
        assert_eq!(
            result.decision()["applied_policies"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            result.decision()["applied_policies"][0]["policy_key"],
            "second"
        );
        service
            .change_group_member(
                &context,
                (group, user),
                true,
                None,
                (source, source),
                "restore",
            )
            .await
            .unwrap();
        assert_eq!(
            evaluate().await.unwrap().decision()["applied_policies"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[sqlx::test]
    async fn group_policy_and_quota_creation_enforce_scope(pool: PgPool) {
        sqlx::raw_sql(include_str!("../../tests/group_selection_fixture.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../../tests/group_admin_actor.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let context = AdminContext {
            tenant_id: Uuid::from_u128(0x100),
            account_id: Uuid::from_u128(0x99),
            session_id: Uuid::new_v4(),
            role: AdminRole::Admin,
        };
        let policies =
            crate::policy::PolicyEngine::with_cache(pool.clone(), 8, Duration::from_secs(60));
        let service = service(pool).with_policy_engine(policies.clone());
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let group = Uuid::from_u128(0x10);
        let user = Uuid::from_u128(1);
        let policy = service
            .create_resource(
                &context,
                ResourceKind::Policies,
                json!({"policy_key":"new", "revision":1, "group_id":group, "configuration":{}}),
                source,
                source,
                "group-policy",
            )
            .await
            .unwrap();
        assert_eq!(policy.resource["group_id"], group.to_string());
        let quota = service
            .create_resource(
                &context,
                ResourceKind::Quotas,
                json!({"quota_key":"new", "revision":1, "group_id":group,
                   "configuration":{"mode":"hard","limits":{"requests_per_minute":1}}}),
                source,
                source,
                "group-quota",
            )
            .await
            .unwrap();
        assert_eq!(quota.resource["group_id"], group.to_string());
        let invalid = service.create_resource(&context, ResourceKind::Policies,
            json!({"policy_key":"mixed", "revision":1, "group_id":group, "user_id":user, "configuration":{}}),
            source, source, "mixed-policy").await;
        assert!(matches!(invalid, Err(AdminError::InvalidRequest)));
        let invalid_quota = service
            .create_resource(
                &context,
                ResourceKind::Quotas,
                json!({"quota_key":"mixed", "revision":1, "group_id":group, "user_id":user,
                   "configuration":{"mode":"hard","limits":{"requests_per_minute":1}}}),
                source,
                source,
                "mixed-quota",
            )
            .await;
        assert!(matches!(invalid_quota, Err(AdminError::InvalidRequest)));
        let foreign = service.create_resource(&context, ResourceKind::Policies,
            json!({"policy_key":"foreign", "revision":1, "group_id":Uuid::from_u128(0x20), "configuration":{}}),
            source, source, "foreign-policy").await;
        assert!(matches!(foreign, Err(AdminError::NotFound)));
        let foreign_quota = service
            .create_resource(
                &context,
                ResourceKind::Quotas,
                json!({"quota_key":"foreign", "revision":1, "group_id":Uuid::from_u128(0x20),
                "configuration":{"mode":"hard","limits":{"requests_per_minute":1}}}),
                source,
                source,
                "foreign-quota",
            )
            .await;
        assert!(matches!(foreign_quota, Err(AdminError::NotFound)));
        let principal = crate::identity::PrincipalContext {
            tenant_id: context.tenant_id,
            credential_id: Uuid::new_v4(),
            credential_name: "test".into(),
            user_id: Some(user),
            user_name: None,
            device_id: None,
            device_name: None,
            client_id: None,
            client_name: None,
            agent_id: None,
            agent_name: None,
        };
        let selector = ModelSelector::parse("company-default").unwrap();
        let first = policies
            .evaluate(
                &principal,
                &axum::http::HeaderMap::new(),
                &selector,
                chrono::Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(
            first.decision()["applied_policies"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let personal = service
            .create_resource(
                &context,
                ResourceKind::Policies,
                json!({"policy_key":"personal", "revision":1, "user_id":user, "configuration":{}}),
                source,
                source,
                "personal-policy",
            )
            .await
            .unwrap();
        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::Policies, id(&personal)),
                LifecycleInput {
                    version: 1,
                    state: "active".into(),
                },
                source,
                source,
                "personal-activate",
            )
            .await
            .unwrap();
        let refreshed = policies
            .evaluate(
                &principal,
                &axum::http::HeaderMap::new(),
                &selector,
                chrono::Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(
            refreshed.decision()["applied_policies"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            refreshed.decision()["applied_policies"][2]["policy_key"],
            "personal"
        );
    }

    #[sqlx::test]
    async fn quota_activation_validates_and_replaces_one_scope_with_audit(pool: PgPool) {
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
        let service = service(pool.clone());
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let invalid = service
            .create_resource(
                &context,
                ResourceKind::Quotas,
                json!({
                    "quota_key": "global",
                    "revision": 1,
                    "configuration": {"requests": 10}
                }),
                source,
                source,
                "invalid-quota",
            )
            .await;
        assert!(matches!(invalid, Err(AdminError::InvalidRequest)));
        let first = service
            .create_resource(
                &context,
                ResourceKind::Quotas,
                json!({
                    "quota_key": "global",
                    "revision": 1,
                    "configuration": {"mode": "hard", "limits": {"requests_per_minute": 10}}
                }),
                source,
                source,
                "create-quota-one",
            )
            .await
            .unwrap();
        let second = service
            .create_resource(
                &context,
                ResourceKind::Quotas,
                json!({
                    "quota_key": "global",
                    "revision": 2,
                    "configuration": {"mode": "hard", "limits": {"requests_per_minute": 20}}
                }),
                source,
                source,
                "create-quota-two",
            )
            .await
            .unwrap();
        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::Quotas, id(&first)),
                LifecycleInput {
                    version: 1,
                    state: "active".to_owned(),
                },
                source,
                source,
                "activate-quota-one",
            )
            .await
            .unwrap();
        service
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::Quotas, id(&second)),
                LifecycleInput {
                    version: 1,
                    state: "active".to_owned(),
                },
                source,
                source,
                "activate-quota-two",
            )
            .await
            .unwrap();
        let evidence: Value = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'first', (SELECT lifecycle_state FROM quotas WHERE id = $1),
                'second', (SELECT lifecycle_state FROM quotas WHERE id = $2),
                'active_count', (SELECT count(*) FROM quotas
                    WHERE tenant_id = $3 AND lifecycle_state = 'active'),
                'replacement_audits', (SELECT count(*) FROM audit_logs
                    WHERE tenant_id = $3 AND action = 'admin.quota.disabled')
            )
            "#,
        )
        .bind(id(&first))
        .bind(id(&second))
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(evidence["first"], "disabled");
        assert_eq!(evidence["second"], "active");
        assert_eq!(evidence["active_count"], 1);
        assert_eq!(evidence["replacement_audits"], 1);
    }

    #[sqlx::test]
    async fn concurrent_policy_activation_keeps_exactly_one_active_scope(pool: PgPool) {
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
        let service = service(pool.clone());
        let source = IpAddr::from_str("127.0.0.1").unwrap();
        let first = service
            .create_resource(
                &context,
                ResourceKind::Policies,
                json!({"policy_key":"concurrent","revision":1,"configuration":{}}),
                source,
                source,
                "create-concurrent-one",
            )
            .await
            .unwrap();
        let second = service
            .create_resource(
                &context,
                ResourceKind::Policies,
                json!({"policy_key":"concurrent","revision":2,"configuration":{}}),
                source,
                source,
                "create-concurrent-two",
            )
            .await
            .unwrap();
        let first_service = service.clone();
        let first_context = context.clone();
        let (first_result, second_result) = tokio::join!(
            first_service.set_registry_lifecycle(
                &first_context,
                ResourceTarget::new(ResourceKind::Policies, id(&first)),
                LifecycleInput {
                    version: 1,
                    state: "active".to_owned()
                },
                source,
                source,
                "activate-concurrent-one",
            ),
            service.set_registry_lifecycle(
                &context,
                ResourceTarget::new(ResourceKind::Policies, id(&second)),
                LifecycleInput {
                    version: 1,
                    state: "active".to_owned()
                },
                source,
                source,
                "activate-concurrent-two",
            )
        );
        first_result.unwrap();
        second_result.unwrap();
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM policies WHERE tenant_id = $1 AND lifecycle_state = 'active'",
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(active, 1);
    }

    #[sqlx::test]
    async fn task_classification_resources_are_tenant_safe_audited_and_role_guarded(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let other_tenant_id = Uuid::new_v4();
        let actor_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO admin_accounts (id, tenant_id, display_name, role) VALUES ($1, $2, 'operator', 'operator')",
        )
        .bind(actor_id)
        .bind(tenant_id)
        .execute(&pool)
        .await
        .unwrap();
        let provider_id = Uuid::new_v4();
        let model_id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO providers (
                id, tenant_id, provider_key, display_name, provider_type,
                base_url, credential_ref, supported_operations, lifecycle_state
            ) VALUES ($1, $2, 'task-provider', 'provider', 'openai',
                      'https://example.invalid', 'env://PROVIDER_KEY',
                      ARRAY['openai_responses'], 'active')
            "#,
        )
        .bind(provider_id)
        .bind(tenant_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO models (
                id, tenant_id, provider_id, public_model_id, provider_model_id,
                display_name, supported_operations, lifecycle_state
            ) VALUES ($1, $2, $3, 'task-model', 'provider-task-model',
                      'task model', ARRAY['openai_responses'], 'active')
            "#,
        )
        .bind(model_id)
        .bind(tenant_id)
        .bind(provider_id)
        .execute(&pool)
        .await
        .unwrap();
        let operator = AdminContext {
            tenant_id,
            account_id: actor_id,
            session_id: Uuid::new_v4(),
            role: AdminRole::Operator,
        };
        let admin = AdminContext {
            role: AdminRole::Admin,
            ..operator.clone()
        };
        let other_tenant = AdminContext {
            tenant_id: other_tenant_id,
            role: AdminRole::Viewer,
            ..operator.clone()
        };
        let service = service(pool.clone());
        let source = IpAddr::from_str("127.0.0.1").unwrap();

        let profile = service
            .create_resource(
                &operator,
                ResourceKind::ModelTaskProfiles,
                json!({
                    "model_id": model_id,
                    "taxonomy_version": "task-taxonomy-v1",
                    "task_kind": "debugging",
                    "suitability_score": 90,
                    "confidence": 0.95,
                    "source": "manual",
                    "source_ref": "operator-review",
                    "rationale": "Validated against the fixed case set"
                }),
                source,
                source,
                "create-task-profile",
            )
            .await
            .unwrap();
        assert!(matches!(
            service
                .get_resource(&other_tenant, ResourceKind::ModelTaskProfiles, id(&profile))
                .await,
            Err(AdminError::NotFound)
        ));
        assert!(matches!(
            service
                .set_registry_lifecycle(
                    &operator,
                    ResourceTarget::new(ResourceKind::ModelTaskProfiles, id(&profile)),
                    LifecycleInput {
                        version: 1,
                        state: "active".to_owned()
                    },
                    source,
                    source,
                    "operator-activate-profile",
                )
                .await,
            Err(AdminError::Forbidden)
        ));
        service
            .set_registry_lifecycle(
                &admin,
                ResourceTarget::new(ResourceKind::ModelTaskProfiles, id(&profile)),
                LifecycleInput {
                    version: 1,
                    state: "active".to_owned(),
                },
                source,
                source,
                "admin-activate-profile",
            )
            .await
            .unwrap();

        let config = service
            .create_resource(
                &operator,
                ResourceKind::TaskClassifierConfigs,
                json!({
                    "taxonomy_version": "task-taxonomy-v1",
                    "classifier_model_id": model_id,
                    "mode": "shadow",
                    "minimum_confidence": 0.8,
                    "timeout_ms": 500
                }),
                source,
                source,
                "create-classifier-config",
            )
            .await
            .unwrap();
        assert!(matches!(
            service
                .update_resource(
                    &operator,
                    ResourceTarget::new(ResourceKind::TaskClassifierConfigs, id(&config)),
                    json!({"version": 1, "mode": "enforced"}),
                    source,
                    source,
                    "operator-enforce-classifier",
                )
                .await,
            Err(AdminError::Forbidden)
        ));
        service
            .update_resource(
                &admin,
                ResourceTarget::new(ResourceKind::TaskClassifierConfigs, id(&config)),
                json!({"version": 1, "mode": "enforced"}),
                source,
                source,
                "admin-enforce-classifier",
            )
            .await
            .unwrap();

        let audits: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*) FROM audit_logs
            WHERE tenant_id = $1 AND target_type IN (
                'model_task_profile', 'task_classifier_config'
            )
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audits, 4);
    }
}
