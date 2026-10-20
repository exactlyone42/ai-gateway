use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{Error as _, MapAccess, Visitor},
};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use thiserror::Error;
use uuid::Uuid;

use crate::ledger::ModelSelectionSnapshot;

const QUALITY_PREFIX: &str = "quality:";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GatewayOperation {
    AnthropicMessages,
    AnthropicCountTokens,
    OpenAiResponses,
    OpenAiChatCompletions,
    GeminiGenerateContent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderDialect {
    Anthropic,
    OpenAi,
    Gemini,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
    Gemini,
    Xai,
    Vllm,
    Ollama,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::Xai => "xai",
            Self::Vllm => "vllm",
            Self::Ollama => "ollama",
        }
    }

    pub fn database_type(self) -> &'static str {
        match self {
            Self::Vllm | Self::Ollama => "local",
            _ => self.as_str(),
        }
    }

    pub fn dialect(self) -> ProviderDialect {
        match self {
            Self::Anthropic => ProviderDialect::Anthropic,
            Self::Gemini => ProviderDialect::Gemini,
            Self::OpenAi | Self::Xai | Self::Vllm | Self::Ollama => ProviderDialect::OpenAi,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalOperation {
    Generate,
    CountTokens,
}

impl GatewayOperation {
    pub fn dialect(self) -> ProviderDialect {
        match self {
            Self::AnthropicMessages | Self::AnthropicCountTokens => ProviderDialect::Anthropic,
            Self::OpenAiResponses | Self::OpenAiChatCompletions => ProviderDialect::OpenAi,
            Self::GeminiGenerateContent => ProviderDialect::Gemini,
        }
    }

    pub fn canonical(self) -> CanonicalOperation {
        match self {
            Self::AnthropicMessages
            | Self::OpenAiResponses
            | Self::OpenAiChatCompletions
            | Self::GeminiGenerateContent => CanonicalOperation::Generate,
            Self::AnthropicCountTokens => CanonicalOperation::CountTokens,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "anthropic_messages",
            Self::AnthropicCountTokens => "anthropic_count_tokens",
            Self::OpenAiResponses => "openai_responses",
            Self::OpenAiChatCompletions => "openai_chat_completions",
            Self::GeminiGenerateContent => "gemini_generate_content",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "anthropic_messages" => Some(Self::AnthropicMessages),
            "anthropic_count_tokens" => Some(Self::AnthropicCountTokens),
            "openai_responses" => Some(Self::OpenAiResponses),
            "openai_chat_completions" => Some(Self::OpenAiChatCompletions),
            "gemini_generate_content" => Some(Self::GeminiGenerateContent),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QualityTier {
    Economy,
    Standard,
    Advanced,
    Premium,
}

impl QualityTier {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "economy" => Some(Self::Economy),
            "standard" => Some(Self::Standard),
            "advanced" => Some(Self::Advanced),
            "premium" => Some(Self::Premium),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Economy => "economy",
            Self::Standard => "standard",
            Self::Advanced => "advanced",
            Self::Premium => "premium",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelSelector {
    Model(String),
    Quality(QualityTier),
}

impl ModelSelector {
    pub fn parse(value: &str) -> Result<Self, RegistryError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(RegistryError::InvalidSelector);
        }
        if let Some(quality) = value.strip_prefix(QUALITY_PREFIX) {
            return QualityTier::parse(quality)
                .map(Self::Quality)
                .ok_or(RegistryError::InvalidSelector);
        }
        Ok(Self::Model(value.to_owned()))
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Model(_) => "model",
            Self::Quality(_) => "quality",
        }
    }

    fn value(&self) -> &str {
        match self {
            Self::Model(value) => value,
            Self::Quality(value) => value.as_str(),
        }
    }

    pub fn requested_value(&self) -> String {
        match self {
            Self::Model(value) => value.clone(),
            Self::Quality(value) => format!("{QUALITY_PREFIX}{}", value.as_str()),
        }
    }

    pub fn quality(&self) -> Option<&'static str> {
        match self {
            Self::Model(_) => None,
            Self::Quality(value) => Some(value.as_str()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CapabilityRequirements {
    pub streaming: bool,
    pub tools: bool,
    pub reasoning: bool,
    pub images: bool,
    pub structured_output: bool,
}

impl CapabilityRequirements {
    pub fn from_request(operation: GatewayOperation, request: &Value) -> Self {
        let content = match operation {
            GatewayOperation::AnthropicMessages | GatewayOperation::AnthropicCountTokens => {
                request.get("messages")
            }
            GatewayOperation::OpenAiResponses => request.get("input"),
            GatewayOperation::OpenAiChatCompletions => request.get("messages"),
            GatewayOperation::GeminiGenerateContent => None,
        };
        let tools = request
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| !tools.is_empty())
            || content.is_some_and(|value| {
                contains_type(
                    value,
                    &[
                        "tool_use",
                        "tool_result",
                        "function_call",
                        "function_call_output",
                    ],
                )
            });
        let reasoning = request.get("thinking").is_some_and(nonempty_value)
            || request.get("reasoning").is_some_and(nonempty_value)
            || request.get("reasoning_effort").is_some_and(nonempty_value);
        let images = content
            .is_some_and(|value| contains_type(value, &["image", "image_url", "input_image"]));
        let structured_output = request.get("response_format").is_some_and(nonempty_value)
            || request.pointer("/text/format").is_some_and(nonempty_value)
            || request
                .pointer("/output_config/format")
                .is_some_and(nonempty_value);
        Self {
            streaming: request.get("stream").and_then(Value::as_bool) == Some(true),
            tools,
            reasoning,
            images,
            structured_output,
        }
    }
}

pub fn parse_request(body: &[u8]) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    let request = StrictObject::deserialize(&mut deserializer)?.0;
    deserializer.end()?;
    Ok(request)
}

pub fn rewrite_model(
    original: &[u8],
    request: &mut Value,
    provider_model_id: &str,
) -> Result<Vec<u8>, RegistryError> {
    let current = request
        .get("model")
        .and_then(Value::as_str)
        .ok_or(RegistryError::InvalidSelector)?;
    if current == provider_model_id {
        return Ok(original.to_vec());
    }
    request["model"] = Value::String(provider_model_id.to_owned());
    serde_json::to_vec(request).map_err(RegistryError::RequestEncoding)
}

struct StrictObject(Value);

impl<'de> Deserialize<'de> for StrictObject {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(StrictObjectVisitor)
    }
}

struct StrictObjectVisitor;

impl<'de> Visitor<'de> for StrictObjectVisitor {
    type Value = StrictObject;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON object with exactly one model field")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = serde_json::Map::new();
        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            if object.insert(key.clone(), value).is_some() {
                return Err(A::Error::custom(format!("duplicate top-level field {key}")));
            }
        }
        if !object.get("model").is_some_and(Value::is_string) {
            return Err(A::Error::custom("model must be a string"));
        }
        Ok(StrictObject(Value::Object(object)))
    }
}

fn contains_type(value: &Value, types: &[&str]) -> bool {
    match value {
        Value::Array(values) => values.iter().any(|value| contains_type(value, types)),
        Value::Object(object) => {
            object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| types.contains(&kind))
                || object.values().any(|value| contains_type(value, types))
        }
        _ => false,
    }
}

fn nonempty_value(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Number(_) => true,
    }
}

#[derive(Clone)]
pub struct ProviderProfile {
    pub credential_ref: String,
    pub provider_key: String,
    pub base_url: String,
    pub operations: Vec<GatewayOperation>,
    pub target_dialect: ProviderDialect,
    pub kind: ProviderKind,
    allowed_tenants: Arc<HashSet<Uuid>>,
    authorization: ProviderAuthorization,
}

#[derive(Clone)]
enum ProviderAuthorization {
    Bearer(Arc<str>),
    ApiKey(Arc<str>),
    None,
}

impl ProviderProfile {
    pub fn bearer(
        credential_ref: impl Into<String>,
        provider_key: impl Into<String>,
        base_url: impl Into<String>,
        target_dialect: ProviderDialect,
        operations: Vec<GatewayOperation>,
        allowed_tenants: Vec<Uuid>,
        secret: impl Into<Arc<str>>,
    ) -> Self {
        Self::bearer_for(
            match target_dialect {
                ProviderDialect::Anthropic => ProviderKind::Anthropic,
                ProviderDialect::OpenAi => ProviderKind::OpenAi,
                ProviderDialect::Gemini => ProviderKind::Gemini,
            },
            credential_ref,
            provider_key,
            base_url,
            operations,
            allowed_tenants,
            secret,
        )
    }

    pub fn bearer_for(
        kind: ProviderKind,
        credential_ref: impl Into<String>,
        provider_key: impl Into<String>,
        base_url: impl Into<String>,
        operations: Vec<GatewayOperation>,
        allowed_tenants: Vec<Uuid>,
        secret: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            credential_ref: credential_ref.into(),
            provider_key: provider_key.into(),
            base_url: canonical_base_url(base_url.into()),
            operations,
            target_dialect: kind.dialect(),
            kind,
            allowed_tenants: Arc::new(allowed_tenants.into_iter().collect()),
            authorization: ProviderAuthorization::Bearer(secret.into()),
        }
    }

    pub fn api_key(
        credential_ref: impl Into<String>,
        provider_key: impl Into<String>,
        base_url: impl Into<String>,
        target_dialect: ProviderDialect,
        operations: Vec<GatewayOperation>,
        allowed_tenants: Vec<Uuid>,
        secret: impl Into<Arc<str>>,
    ) -> Self {
        Self::api_key_for(
            match target_dialect {
                ProviderDialect::Anthropic => ProviderKind::Anthropic,
                ProviderDialect::OpenAi => ProviderKind::OpenAi,
                ProviderDialect::Gemini => ProviderKind::Gemini,
            },
            credential_ref,
            provider_key,
            base_url,
            operations,
            allowed_tenants,
            secret,
        )
    }

    pub fn api_key_for(
        kind: ProviderKind,
        credential_ref: impl Into<String>,
        provider_key: impl Into<String>,
        base_url: impl Into<String>,
        operations: Vec<GatewayOperation>,
        allowed_tenants: Vec<Uuid>,
        secret: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            credential_ref: credential_ref.into(),
            provider_key: provider_key.into(),
            base_url: canonical_base_url(base_url.into()),
            operations,
            target_dialect: kind.dialect(),
            kind,
            allowed_tenants: Arc::new(allowed_tenants.into_iter().collect()),
            authorization: ProviderAuthorization::ApiKey(secret.into()),
        }
    }

    pub fn unauthenticated_for(
        kind: ProviderKind,
        credential_ref: impl Into<String>,
        provider_key: impl Into<String>,
        base_url: impl Into<String>,
        operations: Vec<GatewayOperation>,
        allowed_tenants: Vec<Uuid>,
    ) -> Self {
        Self {
            credential_ref: credential_ref.into(),
            provider_key: provider_key.into(),
            base_url: canonical_base_url(base_url.into()),
            operations,
            target_dialect: kind.dialect(),
            kind,
            allowed_tenants: Arc::new(allowed_tenants.into_iter().collect()),
            authorization: ProviderAuthorization::None,
        }
    }

    pub fn bearer_secret(&self) -> Option<&str> {
        match &self.authorization {
            ProviderAuthorization::Bearer(secret) => Some(secret),
            ProviderAuthorization::ApiKey(_) | ProviderAuthorization::None => None,
        }
    }

    pub fn api_key_secret(&self) -> Option<&str> {
        match &self.authorization {
            ProviderAuthorization::Bearer(_) | ProviderAuthorization::None => None,
            ProviderAuthorization::ApiKey(secret) => Some(secret),
        }
    }
}

#[derive(Clone)]
pub struct ProviderProfiles {
    by_ref: Arc<HashMap<String, ProviderProfile>>,
}

impl ProviderProfiles {
    pub fn new(profiles: impl IntoIterator<Item = ProviderProfile>) -> Result<Self, RegistryError> {
        let mut by_ref = HashMap::new();
        for profile in profiles {
            if profile.credential_ref.is_empty()
                || profile.provider_key.is_empty()
                || profile.base_url.is_empty()
                || profile.operations.is_empty()
                || profile.allowed_tenants.is_empty()
                || profile.target_dialect != profile.kind.dialect()
                || by_ref
                    .insert(profile.credential_ref.clone(), profile)
                    .is_some()
            {
                return Err(RegistryError::InvalidProviderProfile);
            }
        }
        Ok(Self {
            by_ref: Arc::new(by_ref),
        })
    }

    fn resolve(
        &self,
        tenant_id: Uuid,
        credential_ref: &str,
        provider_key: &str,
        provider_type: &str,
        base_url: &str,
    ) -> Result<ProviderProfile, RegistryError> {
        self.by_ref
            .get(credential_ref)
            .filter(|profile| profile.allowed_tenants.contains(&tenant_id))
            .filter(|profile| profile.provider_key == provider_key)
            .filter(|profile| profile.kind.database_type() == provider_type)
            .filter(|profile| profile.base_url == canonical_base_url(base_url.to_owned()))
            .cloned()
            .ok_or(RegistryError::UntrustedProvider)
    }

    pub fn validates(
        &self,
        tenant_id: Uuid,
        credential_ref: &str,
        provider_key: &str,
        provider_type: &str,
        base_url: &str,
        operations: &[String],
    ) -> bool {
        !operations.is_empty()
            && self
                .resolve(
                    tenant_id,
                    credential_ref,
                    provider_key,
                    provider_type,
                    base_url,
                )
                .is_ok_and(|profile| {
                    operations.iter().all(|operation| {
                        GatewayOperation::parse(operation)
                            .is_some_and(|operation| profile.operations.contains(&operation))
                    })
                })
    }
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct ModelMetadata {
    pub id: String,
    pub display_name: String,
    pub description: Option<String>,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub supported_operations: Vec<String>,
    pub supports_streaming: bool,
    pub supports_tools: bool,
    pub supports_reasoning: bool,
    pub supports_images: bool,
    pub supports_structured_output: bool,
    pub provider_key: String,
    #[serde(skip)]
    provider_type: String,
    #[serde(skip)]
    base_url: String,
    #[serde(skip)]
    credential_ref: String,
}

#[derive(Clone)]
pub struct ModelSelection {
    pub selector: ModelSelector,
    pub alias_id: Option<Uuid>,
    pub alias_version: Option<i64>,
    pub model_id: Uuid,
    pub model_version: i64,
    pub public_model_id: String,
    pub provider_model_id: String,
    pub provider_id: Uuid,
    pub provider_version: i64,
    pub provider_key: String,
    pub max_output_tokens: Option<i64>,
    pub cost_metadata: Value,
    pub profile: ProviderProfile,
    pub(crate) model_supported_operations: Vec<GatewayOperation>,
    pub(crate) provider_supported_operations: Vec<GatewayOperation>,
}

#[derive(FromRow)]
struct SelectionRow {
    alias_id: Option<Uuid>,
    alias_version: Option<i64>,
    quality_tier: Option<String>,
    model_id: Uuid,
    model_version: i64,
    public_model_id: String,
    provider_model_id: String,
    provider_id: Uuid,
    provider_version: i64,
    provider_key: String,
    provider_type: String,
    base_url: String,
    credential_ref: String,
    supports_streaming: bool,
    supports_tools: bool,
    supports_reasoning: bool,
    supports_images: bool,
    supports_structured_output: bool,
    max_output_tokens: Option<i64>,
    cost_metadata: Value,
    model_supported_operations: Vec<String>,
    provider_supported_operations: Vec<String>,
}

#[derive(Clone)]
pub struct ModelRegistry {
    pool: PgPool,
    profiles: ProviderProfiles,
    development_models: Arc<[String]>,
}

impl ModelRegistry {
    pub fn new(pool: PgPool, profiles: ProviderProfiles, development_models: Vec<String>) -> Self {
        Self {
            pool,
            profiles,
            development_models: development_models.into(),
        }
    }

    pub async fn list(
        &self,
        tenant_id: Uuid,
        development_bootstrap: bool,
    ) -> Result<Vec<ModelMetadata>, RegistryError> {
        if development_bootstrap {
            return Ok(self
                .development_models
                .iter()
                .map(|id| ModelMetadata {
                    id: id.clone(),
                    display_name: id.clone(),
                    description: None,
                    context_window: None,
                    max_output_tokens: None,
                    supported_operations: vec![
                        GatewayOperation::OpenAiResponses.as_str().to_owned(),
                        GatewayOperation::OpenAiChatCompletions.as_str().to_owned(),
                        GatewayOperation::AnthropicMessages.as_str().to_owned(),
                    ],
                    supports_streaming: false,
                    supports_tools: false,
                    supports_reasoning: false,
                    supports_images: false,
                    supports_structured_output: false,
                    provider_key: "gateway".to_owned(),
                    provider_type: String::new(),
                    base_url: String::new(),
                    credential_ref: String::new(),
                })
                .collect());
        }
        if tenant_id.is_nil() {
            return Err(RegistryError::ModelUnavailable);
        }
        let mut models = sqlx::query_as::<_, ModelMetadata>(
            r#"
            SELECT m.public_model_id AS id, m.display_name, m.description,
                   m.context_window, m.max_output_tokens, m.supported_operations,
                   m.supports_streaming,
                   m.supports_tools, m.supports_reasoning, m.supports_images,
                   m.supports_structured_output, p.provider_key, p.base_url,
                   p.credential_ref, p.provider_type
            FROM models m
            JOIN providers p ON (p.tenant_id, p.id) = (m.tenant_id, m.provider_id)
            WHERE m.tenant_id = $1 AND m.lifecycle_state = 'active'
              AND p.lifecycle_state = 'active'
            ORDER BY m.public_model_id
            "#,
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await?;
        models.retain(|model| {
            model.supported_operations.iter().any(|operation| {
                GatewayOperation::parse(operation).is_some_and(|operation| {
                    self.profiles
                        .resolve(
                            tenant_id,
                            &model.credential_ref,
                            &model.provider_key,
                            &model.provider_type,
                            &model.base_url,
                        )
                        .is_ok_and(|profile| profile.operations.contains(&operation))
                })
            })
        });
        Ok(models)
    }

    #[tracing::instrument(name = "router.select", skip_all)]
    #[cfg(test)]
    pub async fn select(
        &self,
        tenant_id: Uuid,
        development_bootstrap: bool,
        operation: GatewayOperation,
        selector: ModelSelector,
        required: CapabilityRequirements,
    ) -> Result<ModelSelection, RegistryError> {
        if development_bootstrap {
            return self.development_selection(operation, selector, required);
        }
        if tenant_id.is_nil() {
            return Err(RegistryError::ModelUnavailable);
        }
        let row = sqlx::query_as::<_, SelectionRow>(SELECTION_SQL)
            .bind(tenant_id)
            .bind(operation.as_str())
            .bind(selector.kind())
            .bind(selector.value())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(RegistryError::ModelUnavailable)?;
        let selection = self.selection_from_row(tenant_id, selector, row, required)?;
        if !selection.supports_target_operation(operation) {
            return Err(RegistryError::ModelUnavailable);
        }
        tracing::info!(
            event.name = "router.selected",
            gen_ai.system = %selection.provider_key,
            gen_ai.request.model = %selection.public_model_id,
            "model route selected"
        );
        Ok(selection)
    }

    pub async fn candidates(
        &self,
        tenant_id: Uuid,
        development_bootstrap: bool,
        operation: GatewayOperation,
        selector: ModelSelector,
        required: CapabilityRequirements,
    ) -> Result<Vec<ModelSelection>, RegistryError> {
        if development_bootstrap {
            return self
                .development_selection(operation, selector, required)
                .map(|selection| vec![selection]);
        }
        if tenant_id.is_nil() {
            return Err(RegistryError::ModelUnavailable);
        }
        let rows = if matches!(selector, ModelSelector::Model(_)) {
            sqlx::query_as::<_, SelectionRow>(SELECTION_SQL)
                .bind(tenant_id)
                .bind(operation.as_str())
                .bind(selector.kind())
                .bind(selector.value())
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query_as::<_, SelectionRow>(QUALITY_CANDIDATES_SQL)
                .bind(tenant_id)
                .bind(operation.as_str())
                .bind(selector.value())
                .fetch_all(&self.pool)
                .await?
        };
        let mut selections = Vec::new();
        let mut capability_error = None;
        for row in rows {
            let row_selector = match row.quality_tier.as_deref() {
                Some(quality) => ModelSelector::Quality(
                    QualityTier::parse(quality).ok_or(RegistryError::ModelUnavailable)?,
                ),
                None => selector.clone(),
            };
            match self.selection_from_row(tenant_id, row_selector, row, required) {
                Ok(selection) => selections.push(selection),
                Err(error @ RegistryError::CapabilityMismatch(_)) => capability_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        if selections.is_empty() {
            return Err(capability_error.unwrap_or(RegistryError::ModelUnavailable));
        }
        Ok(selections)
    }

    fn selection_from_row(
        &self,
        tenant_id: Uuid,
        selector: ModelSelector,
        row: SelectionRow,
        required: CapabilityRequirements,
    ) -> Result<ModelSelection, RegistryError> {
        ensure_capabilities(&row, required)?;
        let profile = self.profiles.resolve(
            tenant_id,
            &row.credential_ref,
            &row.provider_key,
            &row.provider_type,
            &row.base_url,
        )?;
        let model_supported_operations = parse_operations(&row.model_supported_operations)?;
        let provider_supported_operations = parse_operations(&row.provider_supported_operations)?;
        Ok(ModelSelection {
            selector,
            alias_id: row.alias_id,
            alias_version: row.alias_version,
            model_id: row.model_id,
            model_version: row.model_version,
            public_model_id: row.public_model_id,
            provider_model_id: row.provider_model_id,
            provider_id: row.provider_id,
            provider_version: row.provider_version,
            provider_key: row.provider_key,
            max_output_tokens: row.max_output_tokens,
            cost_metadata: row.cost_metadata,
            profile,
            model_supported_operations,
            provider_supported_operations,
        })
    }

    fn development_selection(
        &self,
        operation: GatewayOperation,
        selector: ModelSelector,
        required: CapabilityRequirements,
    ) -> Result<ModelSelection, RegistryError> {
        let model = match &selector {
            ModelSelector::Model(model) => model.clone(),
            ModelSelector::Quality(_) => return Err(RegistryError::ModelUnavailable),
        };
        if !self
            .development_models
            .iter()
            .any(|allowed| allowed == &model)
        {
            return Err(RegistryError::ModelUnavailable);
        }
        let target_operation = operation;
        let profile = self
            .profiles
            .by_ref
            .values()
            .find(|profile| {
                profile.allowed_tenants.contains(&Uuid::nil())
                    && profile.operations.contains(&target_operation)
            })
            .cloned()
            .ok_or(RegistryError::UntrustedProvider)?;
        let _ = required;
        Ok(ModelSelection {
            selector,
            alias_id: None,
            alias_version: None,
            model_id: Uuid::nil(),
            model_version: 1,
            public_model_id: model.clone(),
            provider_model_id: model,
            provider_id: Uuid::nil(),
            provider_version: 1,
            provider_key: profile.provider_key.clone(),
            max_output_tokens: None,
            cost_metadata: serde_json::json!({}),
            profile,
            model_supported_operations: vec![target_operation],
            provider_supported_operations: vec![target_operation],
        })
    }
}

impl ModelSelection {
    pub fn supports_target_operation(&self, operation: GatewayOperation) -> bool {
        self.model_supported_operations.contains(&operation)
            && self.provider_supported_operations.contains(&operation)
            && self.profile.operations.contains(&operation)
    }
    pub fn record_selected(&self) {
        tracing::info!(
            event.name = "router.selected",
            gen_ai.system = %self.provider_key,
            gen_ai.request.model = %self.public_model_id,
            "model route selected"
        );
    }

    pub fn ledger_snapshot(&self) -> Option<ModelSelectionSnapshot> {
        (!self.model_id.is_nil() && !self.provider_id.is_nil()).then(|| ModelSelectionSnapshot {
            selector_kind: self.selector.kind().to_owned(),
            selector_value: self.selector.value().to_owned(),
            alias_id: self.alias_id,
            alias_version: self.alias_version,
            model_id: self.model_id,
            model_version: self.model_version,
            provider_id: self.provider_id,
            provider_version: self.provider_version,
            provider_model_id: self.provider_model_id.clone(),
            cost_metadata: self.cost_metadata.clone(),
        })
    }
}

fn ensure_capabilities(
    row: &SelectionRow,
    required: CapabilityRequirements,
) -> Result<(), RegistryError> {
    let checks = [
        (required.streaming, row.supports_streaming, "streaming"),
        (required.tools, row.supports_tools, "tools"),
        (required.reasoning, row.supports_reasoning, "reasoning"),
        (required.images, row.supports_images, "images"),
        (
            required.structured_output,
            row.supports_structured_output,
            "structured output",
        ),
    ];
    checks
        .into_iter()
        .find(|(needed, supported, _)| *needed && !*supported)
        .map(|(_, _, capability)| Err(RegistryError::CapabilityMismatch(capability)))
        .unwrap_or(Ok(()))
}

fn parse_operations(values: &[String]) -> Result<Vec<GatewayOperation>, RegistryError> {
    values
        .iter()
        .map(|value| GatewayOperation::parse(value).ok_or(RegistryError::InvalidProviderProfile))
        .collect()
}

fn canonical_base_url(value: String) -> String {
    value.trim_end_matches('/').to_owned()
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("model selector is invalid")]
    InvalidSelector,
    #[error("no active model matches this request")]
    ModelUnavailable,
    #[error("selected model does not support {0}")]
    CapabilityMismatch(&'static str),
    #[error("provider is not bound to a trusted server-side profile")]
    UntrustedProvider,
    #[error("provider profile configuration is invalid")]
    InvalidProviderProfile,
    #[error("provider request could not be encoded")]
    RequestEncoding(#[source] serde_json::Error),
    #[error("model registry is unavailable")]
    Database(#[from] sqlx::Error),
}

const SELECTION_SQL: &str = r#"
    WITH candidate AS (
        SELECT NULL::uuid AS alias_id, NULL::bigint AS alias_version,
               NULL::text AS quality_tier,
               m.id AS model_id, m.version AS model_version,
               m.public_model_id, m.provider_model_id, m.provider_id,
               m.supports_streaming, m.supports_tools, m.supports_reasoning,
               m.supports_images, m.supports_structured_output,
               m.max_output_tokens, m.cost_metadata,
               m.supported_operations AS model_supported_operations
        FROM models m
        WHERE $3 = 'model' AND m.tenant_id = $1
          AND m.public_model_id = $4 AND m.lifecycle_state = 'active'
        UNION ALL
        SELECT a.id, a.version, a.quality_tier, m.id, m.version,
               m.public_model_id, m.provider_model_id, m.provider_id,
               m.supports_streaming, m.supports_tools, m.supports_reasoning,
               m.supports_images, m.supports_structured_output,
               m.max_output_tokens, m.cost_metadata,
               m.supported_operations AS model_supported_operations
        FROM model_aliases a
        JOIN models m ON (m.tenant_id, m.id) = (a.tenant_id, a.model_id)
        WHERE $3 = 'quality' AND a.tenant_id = $1 AND a.operation = $2
          AND a.quality_tier = $4 AND a.lifecycle_state = 'active'
          AND m.lifecycle_state = 'active'
    )
    SELECT c.*, p.version AS provider_version, p.provider_key,
           p.base_url, p.credential_ref, p.provider_type,
           p.supported_operations AS provider_supported_operations
    FROM candidate c
    JOIN providers p ON (p.tenant_id, p.id) = ($1, c.provider_id)
    WHERE p.lifecycle_state = 'active'
    ORDER BY c.public_model_id, c.model_id
    LIMIT 1
"#;

const QUALITY_CANDIDATES_SQL: &str = r#"
    WITH candidates AS (
    SELECT a.id AS alias_id, a.version AS alias_version, a.quality_tier,
           m.id AS model_id, m.version AS model_version,
           m.public_model_id, m.provider_model_id, m.provider_id,
           m.supports_streaming, m.supports_tools, m.supports_reasoning,
           m.supports_images, m.supports_structured_output,
           m.max_output_tokens, m.cost_metadata
    FROM model_aliases a
    JOIN models m ON (m.tenant_id, m.id) = (a.tenant_id, a.model_id)
    WHERE a.tenant_id = $1 AND a.operation = $2
      AND a.lifecycle_state = 'active' AND m.lifecycle_state = 'active'
    UNION ALL
    SELECT NULL::uuid, NULL::bigint, m.quality_tier,
           m.id, m.version, m.public_model_id, m.provider_model_id, m.provider_id,
           m.supports_streaming, m.supports_tools, m.supports_reasoning,
           m.supports_images, m.supports_structured_output,
           m.max_output_tokens, m.cost_metadata
    FROM models m
    WHERE m.tenant_id = $1 AND m.lifecycle_state = 'active'
      AND m.quality_tier IS NOT NULL
      AND $2 = ANY(m.supported_operations)
      AND NOT EXISTS (
          SELECT 1 FROM model_aliases a
          WHERE (a.tenant_id, a.model_id) = (m.tenant_id, m.id)
            AND a.operation = $2 AND a.lifecycle_state = 'active'
      )
    )
    SELECT c.*,
           p.version AS provider_version, p.provider_key,
           p.base_url, p.credential_ref, p.provider_type,
           m.supported_operations AS model_supported_operations,
           p.supported_operations AS provider_supported_operations
    FROM candidates c
    JOIN models m ON (m.tenant_id, m.id) = ($1, c.model_id)
    JOIN providers p ON (p.tenant_id, p.id) = ($1, c.provider_id)
    WHERE p.lifecycle_state = 'active'
      AND CASE c.quality_tier
            WHEN 'economy' THEN 0 WHEN 'standard' THEN 1
            WHEN 'advanced' THEN 2 WHEN 'premium' THEN 3
          END <= CASE $3
            WHEN 'economy' THEN 0 WHEN 'standard' THEN 1
            WHEN 'advanced' THEN 2 WHEN 'premium' THEN 3
          END
    ORDER BY CASE c.quality_tier
            WHEN 'economy' THEN 0 WHEN 'standard' THEN 1
            WHEN 'advanced' THEN 2 WHEN 'premium' THEN 3
          END DESC, c.public_model_id, c.model_id
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::{
        identity::PrincipalContext,
        ledger::{Ledger, ProviderAttempt, RequestCompletion, RequestStart},
    };

    #[derive(sqlx::FromRow)]
    struct PersistedSelection {
        selected_alias_id: Option<Uuid>,
        selected_alias_version: Option<i64>,
        selected_model_id: Option<Uuid>,
        selected_model_version: Option<i64>,
        selected_provider_id: Option<Uuid>,
        selected_provider_version: Option<i64>,
        provider_model_id: String,
    }

    #[test]
    fn selector_namespace_is_explicit_and_unambiguous() {
        assert_eq!(
            ModelSelector::parse("quality:premium").unwrap(),
            ModelSelector::Quality(QualityTier::Premium)
        );
        assert_eq!(
            ModelSelector::parse("premium").unwrap(),
            ModelSelector::Model("premium".to_owned())
        );
        assert!(matches!(
            ModelSelector::parse("quality:unknown"),
            Err(RegistryError::InvalidSelector)
        ));
    }

    #[test]
    fn provider_profiles_bind_the_exact_origin_and_operation() {
        let tenant_id = Uuid::new_v4();
        let profiles = ProviderProfiles::new([ProviderProfile::bearer(
            "env://PROVIDER_KEY",
            "provider-one",
            "https://provider.example",
            ProviderDialect::OpenAi,
            vec![GatewayOperation::OpenAiResponses],
            vec![tenant_id],
            Arc::<str>::from("secret"),
        )])
        .unwrap();
        assert!(profiles.validates(
            tenant_id,
            "env://PROVIDER_KEY",
            "provider-one",
            "openai",
            "https://provider.example",
            &["openai_responses".to_owned()],
        ));
        assert!(!profiles.validates(
            tenant_id,
            "env://PROVIDER_KEY",
            "provider-one",
            "openai",
            "https://attacker.example",
            &["openai_responses".to_owned()],
        ));
        assert!(!profiles.validates(
            Uuid::new_v4(),
            "env://PROVIDER_KEY",
            "provider-one",
            "openai",
            "https://provider.example",
            &["openai_responses".to_owned()],
        ));
        assert!(!profiles.validates(
            tenant_id,
            "env://PROVIDER_KEY",
            "provider-one",
            "openai",
            "https://provider.example",
            &["anthropic_messages".to_owned()],
        ));
    }

    #[test]
    fn strict_request_analysis_rejects_duplicates_and_detects_capabilities() {
        assert!(parse_request(br#"{"model":"one","model":"two"}"#).is_err());
        let request = parse_request(
            br#"{"model":"quality:advanced","stream":true,"tools":[{"type":"function"}],"input":[{"type":"input_image"}],"reasoning":{"effort":"high"},"text":{"format":{"type":"json_schema"}}}"#,
        )
        .unwrap();
        assert_eq!(
            CapabilityRequirements::from_request(GatewayOperation::OpenAiResponses, &request),
            CapabilityRequirements {
                streaming: true,
                tools: true,
                reasoning: true,
                images: true,
                structured_output: true,
            }
        );
    }

    #[test]
    fn model_rewrite_changes_only_the_selected_field_semantically() {
        let original =
            br#"{ "vendor": {"unknown": 123456789012345678901234567890}, "model": "public" }"#;
        let mut request = parse_request(original).unwrap();
        let rewritten = rewrite_model(original, &mut request, "provider-model").unwrap();
        let actual: Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(actual["model"], "provider-model");
        assert_eq!(
            actual["vendor"]["unknown"].to_string(),
            "123456789012345678901234567890"
        );
    }

    #[sqlx::test]
    async fn selection_is_tenant_scoped_deterministic_and_capability_aware(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let provider_id = Uuid::new_v4();
        let model_id = Uuid::new_v4();
        let alias_id = Uuid::new_v4();
        sqlx::query(
            r#"
            WITH inserted_provider AS (
                INSERT INTO providers (
                    id, tenant_id, provider_key, display_name, provider_type, base_url,
                    credential_ref, supported_operations, lifecycle_state
                ) VALUES ($1, $2, 'provider-one', 'Provider One', 'openai',
                          'https://provider.example', 'env://PROVIDER_ONE_KEY',
                          ARRAY['openai_responses', 'openai_chat_completions'], 'active')
                RETURNING id
            ), inserted_model AS (
                INSERT INTO models (
                    id, tenant_id, provider_id, public_model_id, provider_model_id,
                    display_name, supported_operations, supports_streaming, lifecycle_state
                )
                SELECT $3, $2, id, 'company-model', 'provider-model',
                       'Company Model', ARRAY['openai_responses'], true, 'active'
                FROM inserted_provider
                RETURNING id
            )
            INSERT INTO model_aliases (
                id, tenant_id, operation, quality_tier, model_id, lifecycle_state
            )
            SELECT $4, $2, 'openai_responses', 'premium', id, 'active'
            FROM inserted_model
            "#,
        )
        .bind(provider_id)
        .bind(tenant_id)
        .bind(model_id)
        .bind(alias_id)
        .execute(&pool)
        .await
        .unwrap();
        let profiles = ProviderProfiles::new([ProviderProfile::bearer(
            "env://PROVIDER_ONE_KEY",
            "provider-one",
            "https://provider.example",
            ProviderDialect::OpenAi,
            vec![
                GatewayOperation::OpenAiResponses,
                GatewayOperation::OpenAiChatCompletions,
            ],
            vec![tenant_id],
            Arc::<str>::from("server-secret"),
        )])
        .unwrap();
        let registry = ModelRegistry::new(pool.clone(), profiles, Vec::new());

        let explicit = registry
            .select(
                tenant_id,
                false,
                GatewayOperation::OpenAiResponses,
                ModelSelector::parse("company-model").unwrap(),
                CapabilityRequirements {
                    streaming: true,
                    ..CapabilityRequirements::default()
                },
            )
            .await
            .unwrap();
        let alias = registry
            .select(
                tenant_id,
                false,
                GatewayOperation::OpenAiResponses,
                ModelSelector::parse("quality:premium").unwrap(),
                CapabilityRequirements::default(),
            )
            .await
            .unwrap();
        assert_eq!(explicit.model_id, alias.model_id);
        assert_eq!(alias.alias_id, Some(alias_id));
        assert_eq!(explicit.provider_model_id, "provider-model");
        let listed = registry.list(tenant_id, false).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "company-model");
        assert_eq!(listed[0].display_name, "Company Model");
        assert!(
            registry
                .list(Uuid::new_v4(), false)
                .await
                .unwrap()
                .is_empty()
        );

        let cross_tenant = registry
            .select(
                Uuid::new_v4(),
                false,
                GatewayOperation::OpenAiResponses,
                ModelSelector::parse("company-model").unwrap(),
                CapabilityRequirements::default(),
            )
            .await;
        assert!(matches!(cross_tenant, Err(RegistryError::ModelUnavailable)));
        let tools = registry
            .select(
                tenant_id,
                false,
                GatewayOperation::OpenAiResponses,
                ModelSelector::parse("company-model").unwrap(),
                CapabilityRequirements {
                    tools: true,
                    ..CapabilityRequirements::default()
                },
            )
            .await;
        assert!(matches!(
            tools,
            Err(RegistryError::CapabilityMismatch("tools"))
        ));
        let wrong_operation = registry
            .select(
                tenant_id,
                false,
                GatewayOperation::OpenAiChatCompletions,
                ModelSelector::parse("company-model").unwrap(),
                CapabilityRequirements::default(),
            )
            .await;
        assert!(matches!(
            wrong_operation,
            Err(RegistryError::ModelUnavailable)
        ));

        let spool_dir = std::env::temp_dir().join(format!("registry-ledger-{}", Uuid::new_v4()));
        let ledger = Ledger::open(pool.clone(), spool_dir.clone(), 1024 * 1024, 32 * 1024).unwrap();
        let started_at = Utc::now();
        let snapshot = alias.ledger_snapshot().unwrap();
        let reservation = ledger
            .reserve(RequestStart {
                request_id: "registry-ledger-test".to_owned(),
                session_id: None,
                trace_id: None,
                requested_model: Some("quality:premium".to_owned()),
                workload_class: None,
                urgency: None,
                quality: Some("premium".to_owned()),
                budget_class: None,
                estimated_tokens: Some(1),
                selection: Some(snapshot.clone()),
                policy: None,
                principal: PrincipalContext {
                    tenant_id,
                    credential_id: Uuid::nil(),
                    credential_name: "test".to_owned(),
                    user_id: None,
                    user_name: None,
                    device_id: None,
                    device_name: None,
                    client_id: None,
                    client_name: None,
                    agent_id: None,
                    agent_name: None,
                },
                created_at: started_at,
            })
            .await
            .unwrap();
        reservation
            .finish(RequestCompletion {
                status: "failed".to_owned(),
                selected_model: Some(alias.public_model_id.clone()),
                selected_provider: Some(alias.provider_key.clone()),
                attempts: vec![
                    ProviderAttempt::failed(
                        alias.provider_key,
                        alias.provider_model_id,
                        started_at,
                        Utc::now(),
                        "test_failure".to_owned(),
                    )
                    .with_selection(Some(snapshot)),
                ],
                completed_at: Utc::now(),
            })
            .await
            .unwrap();
        ledger.wait_for_idle().await;
        let persisted: PersistedSelection = sqlx::query_as(
            r#"
            SELECT gr.selected_alias_id, gr.selected_alias_version,
                   gr.selected_model_id, gr.selected_model_version,
                   gr.selected_provider_id, gr.selected_provider_version,
                   pa.provider_model_id
            FROM gateway_requests gr
            JOIN provider_attempts pa
              ON (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id)
            WHERE gr.tenant_id = $1 AND gr.request_id = 'registry-ledger-test'
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(persisted.selected_alias_id, Some(alias_id));
        assert_eq!(persisted.selected_alias_version, Some(1));
        assert_eq!(persisted.selected_model_id, Some(model_id));
        assert_eq!(persisted.selected_model_version, Some(1));
        assert_eq!(persisted.selected_provider_id, Some(provider_id));
        assert_eq!(persisted.selected_provider_version, Some(1));
        assert_eq!(persisted.provider_model_id, "provider-model");
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn quality_candidates_skip_an_incompatible_high_tier(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let provider_id = Uuid::new_v4();
        let premium_id = Uuid::new_v4();
        let standard_id = Uuid::new_v4();
        sqlx::query(
            r#"
            WITH inserted_provider AS (
                INSERT INTO providers (
                    id, tenant_id, provider_key, display_name, provider_type, base_url,
                    credential_ref, supported_operations, lifecycle_state
                ) VALUES ($1, $2, 'fallback-provider', 'Fallback', 'openai',
                          'https://fallback.example', 'env://FALLBACK_KEY',
                          ARRAY['openai_responses'], 'active')
            ), inserted_models AS (
                INSERT INTO models (
                    id, tenant_id, provider_id, public_model_id, provider_model_id,
                    display_name, supported_operations, supports_images, lifecycle_state
                ) VALUES
                    ($3, $2, $1, 'premium-no-image', 'premium', 'Premium',
                     ARRAY['openai_responses'], false, 'active'),
                    ($4, $2, $1, 'standard-image', 'standard', 'Standard',
                     ARRAY['openai_responses'], true, 'active')
            )
            INSERT INTO model_aliases (
                id, tenant_id, operation, quality_tier, model_id, lifecycle_state
            ) VALUES
                ($5, $2, 'openai_responses', 'premium', $3, 'active'),
                ($6, $2, 'openai_responses', 'standard', $4, 'active')
            "#,
        )
        .bind(provider_id)
        .bind(tenant_id)
        .bind(premium_id)
        .bind(standard_id)
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();
        let profiles = ProviderProfiles::new([ProviderProfile::bearer(
            "env://FALLBACK_KEY",
            "fallback-provider",
            "https://fallback.example",
            ProviderDialect::OpenAi,
            vec![GatewayOperation::OpenAiResponses],
            vec![tenant_id],
            Arc::<str>::from("server-secret"),
        )])
        .unwrap();
        let candidates = ModelRegistry::new(pool, profiles, Vec::new())
            .candidates(
                tenant_id,
                false,
                GatewayOperation::OpenAiResponses,
                ModelSelector::parse("quality:premium").unwrap(),
                CapabilityRequirements {
                    images: true,
                    ..CapabilityRequirements::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].model_id, standard_id);
    }

    #[sqlx::test]
    async fn quality_candidates_are_returned_in_downgrade_order(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let provider_id = Uuid::new_v4();
        let model_ids = (0..4).map(|_| Uuid::new_v4()).collect::<Vec<_>>();
        let alias_ids = (0..4).map(|_| Uuid::new_v4()).collect::<Vec<_>>();
        let qualities = vec!["economy", "standard", "advanced", "premium"];
        sqlx::query(
            r#"
            WITH inserted_provider AS (
                INSERT INTO providers (
                    id, tenant_id, provider_key, display_name, provider_type, base_url,
                    credential_ref, supported_operations, lifecycle_state
                ) VALUES ($1, $2, 'provider-one', 'Provider One', 'openai',
                          'https://provider.example', 'env://PROVIDER_ONE_KEY',
                          ARRAY['openai_responses'], 'active')
                RETURNING id
            ), input AS (
                SELECT * FROM unnest($3::uuid[], $4::uuid[], $5::text[])
                    AS row(alias_id, model_id, quality)
            ), inserted_models AS (
                INSERT INTO models (
                    id, tenant_id, provider_id, public_model_id, provider_model_id,
                    display_name, supported_operations, lifecycle_state
                ) SELECT input.model_id, $2, inserted_provider.id,
                         input.quality || '-model', input.quality || '-provider-model',
                         input.quality, ARRAY['openai_responses'], 'active'
                    FROM input CROSS JOIN inserted_provider
                RETURNING id
            )
            INSERT INTO model_aliases (
                id, tenant_id, operation, quality_tier, model_id, lifecycle_state
            ) SELECT input.alias_id, $2, 'openai_responses', input.quality,
                     input.model_id, 'active'
                FROM input JOIN inserted_models ON inserted_models.id = input.model_id
            "#,
        )
        .bind(provider_id)
        .bind(tenant_id)
        .bind(&alias_ids)
        .bind(&model_ids)
        .bind(&qualities)
        .execute(&pool)
        .await
        .unwrap();
        let profiles = ProviderProfiles::new([ProviderProfile::bearer(
            "env://PROVIDER_ONE_KEY",
            "provider-one",
            "https://provider.example",
            ProviderDialect::OpenAi,
            vec![GatewayOperation::OpenAiResponses],
            vec![tenant_id],
            Arc::<str>::from("server-secret"),
        )])
        .unwrap();
        let candidates = ModelRegistry::new(pool, profiles, Vec::new())
            .candidates(
                tenant_id,
                false,
                GatewayOperation::OpenAiResponses,
                ModelSelector::parse("quality:premium").unwrap(),
                CapabilityRequirements::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            candidates
                .iter()
                .filter_map(|candidate| candidate.selector.quality())
                .collect::<Vec<_>>(),
            vec!["premium", "advanced", "standard", "economy"]
        );
    }
}
