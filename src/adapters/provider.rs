use axum::{
    body::Bytes,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
};
use serde_json::{Map, Value, json};

use crate::{
    adapters::ProviderRequestInput,
    adapters::gemini_wire,
    canonical::{
        CanonicalContent, CanonicalRequest, CanonicalRole, CanonicalToolArguments,
        CanonicalToolChoice, CanonicalUsage,
    },
    error::GatewayError,
    logging,
    model_registry::{
        CanonicalOperation, GatewayOperation, ProviderDialect, ProviderKind, ProviderProfile,
        rewrite_model,
    },
    quota::cost_for_usage,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticationMode {
    ApiKey,
    Bearer,
    GoogleApiKey,
    OptionalBearer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsageAvailability {
    Required,
    Optional,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderCapabilities {
    pub streaming: bool,
    pub tools: bool,
    pub structured_output: bool,
    pub reasoning: bool,
    pub usage: UsageAvailability,
}

#[allow(dead_code)]
#[derive(Debug, Eq, PartialEq)]
pub struct ProviderHealthProbe {
    pub method: Method,
    pub path_and_query: String,
    authentication: AuthenticationMode,
}

#[allow(dead_code)]
impl ProviderHealthProbe {
    pub async fn execute(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        profile: &ProviderProfile,
    ) -> Result<StatusCode, GatewayError> {
        let mut headers = HeaderMap::new();
        apply_authentication(&mut headers, self.authentication, profile)?;
        Ok(client
            .request(
                self.method.clone(),
                format!("{}{}", base_url.trim_end_matches('/'), self.path_and_query),
            )
            .headers(headers)
            .send()
            .await?
            .status())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct ProviderRequestPlan {
    pub operation: GatewayOperation,
    pub path_and_query: String,
    pub authentication: AuthenticationMode,
    pub translated: bool,
}

pub struct PreparedProviderRequest {
    plan: ProviderRequestPlan,
    headers: HeaderMap,
    body: Bytes,
}

impl PreparedProviderRequest {
    pub fn path_and_query(&self) -> &str {
        &self.plan.path_and_query
    }

    pub(crate) fn parameter_snapshot(&self) -> Result<Value, GatewayError> {
        let document =
            serde_json::from_slice(&self.body).map_err(|_| GatewayError::InvalidConfiguration)?;
        Ok(crate::request_parameters::snapshot(
            self.plan.operation,
            &document,
        ))
    }

    #[cfg(test)]
    pub fn body(&self) -> &Bytes {
        &self.body
    }

    #[cfg(test)]
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }
}

pub struct NativePayload<'a> {
    body: &'a [u8],
    document: &'a Value,
}

impl<'a> NativePayload<'a> {
    pub(crate) fn new(body: &'a [u8], document: &'a Value) -> Self {
        Self { body, document }
    }
}

pub trait ProviderAdapter: Sync {
    fn kind(&self) -> ProviderKind;

    fn dialect(&self) -> ProviderDialect;

    fn capabilities(&self) -> ProviderCapabilities;

    #[allow(dead_code)]
    fn health(
        &self,
        profile: &ProviderProfile,
        provider_model_id: &str,
    ) -> Result<ProviderHealthProbe, GatewayError>;

    fn send(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError>;

    fn stream(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError>;

    #[allow(dead_code)]
    fn estimate_cost(
        &self,
        usage: &CanonicalUsage,
        cost_metadata: &Value,
    ) -> Result<u64, GatewayError>;

    fn usage(&self, response: &Value) -> Result<Option<CanonicalUsage>, GatewayError>;

    fn target_operation(&self, operation: CanonicalOperation) -> Option<GatewayOperation>;

    fn default_path(&self, operation: GatewayOperation) -> Option<&'static str>;

    fn authentication(&self) -> AuthenticationMode;

    fn allows_client_header(&self, name: &HeaderName) -> bool;

    fn plan_for_target(
        &self,
        target_operation: GatewayOperation,
        source_operation: GatewayOperation,
        provider_model_id: &str,
        stream: bool,
        input: &ProviderRequestInput<'_>,
    ) -> Result<ProviderRequestPlan, GatewayError> {
        if target_operation.dialect() != self.dialect() {
            return Err(GatewayError::InvalidConfiguration);
        }
        let translated = target_operation != source_operation;
        let path_and_query = if !translated {
            input.source_path_and_query.to_owned()
        } else {
            self.target_path(target_operation, provider_model_id, stream)?
        };
        Ok(ProviderRequestPlan {
            operation: target_operation,
            path_and_query,
            authentication: self.authentication(),
            translated,
        })
    }

    fn target_path(
        &self,
        operation: GatewayOperation,
        _provider_model_id: &str,
        _stream: bool,
    ) -> Result<String, GatewayError> {
        self.default_path(operation)
            .map(ToOwned::to_owned)
            .ok_or(GatewayError::InvalidConfiguration)
    }

    fn encode_native(
        &self,
        payload: NativePayload<'_>,
        provider_model_id: &str,
    ) -> Result<Bytes, GatewayError> {
        let mut document = payload.document.clone();
        Ok(Bytes::from(rewrite_model(
            payload.body,
            &mut document,
            provider_model_id,
        )?))
    }

    fn encode_canonical(
        &self,
        _request: &CanonicalRequest,
        _target_operation: GatewayOperation,
        _provider_model_id: &str,
    ) -> Result<Bytes, GatewayError> {
        Err(GatewayError::UnsupportedRequest)
    }
}

fn prepare_request(
    adapter: &dyn ProviderAdapter,
    plan: ProviderRequestPlan,
    body: Bytes,
    profile: &ProviderProfile,
    input: ProviderRequestInput<'_>,
) -> Result<PreparedProviderRequest, GatewayError> {
    let mut headers = HeaderMap::new();
    for (name, value) in input.client_headers {
        let client_bearer = profile.uses_client_bearer() && name == header::AUTHORIZATION;
        let regular_header =
            (!plan.translated || name == header::CONTENT_TYPE || name == header::ACCEPT)
                && adapter.allows_client_header(name)
                && name != header::AUTHORIZATION
                && name.as_str() != "x-api-key"
                && name.as_str() != "x-goog-api-key";
        if client_bearer || regular_header {
            headers.append(name.clone(), value.clone());
        }
    }
    if plan.translated || !headers.contains_key(header::CONTENT_TYPE) {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    if adapter.dialect() == ProviderDialect::Anthropic && plan.translated {
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
    }
    if profile.uses_client_bearer() {
        headers.insert(
            HeaderName::from_static("anthropic-dangerous-direct-browser-access"),
            HeaderValue::from_static("true"),
        );
        let beta_name = HeaderName::from_static("anthropic-beta");
        let beta = headers
            .get(&beta_name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !beta
            .split(',')
            .any(|value| value.trim() == "oauth-2025-04-20")
        {
            let value = if beta.is_empty() {
                "oauth-2025-04-20".to_owned()
            } else {
                format!("{beta},oauth-2025-04-20")
            };
            headers.insert(
                beta_name,
                HeaderValue::from_str(&value).map_err(|_| GatewayError::InvalidConfiguration)?,
            );
        }
    }
    if adapter.dialect() == ProviderDialect::OpenAi
        && !plan.translated
        && let Some(value) = input.idempotency_key
    {
        headers.insert(HeaderName::from_static("idempotency-key"), value);
    }
    logging::inject_context(&mut headers);
    if profile.uses_client_bearer() {
        if !headers.contains_key(header::AUTHORIZATION) {
            return Err(GatewayError::Unauthorized);
        }
    } else {
        apply_authentication(&mut headers, plan.authentication, profile)?;
    }
    Ok(PreparedProviderRequest {
        plan,
        headers,
        body,
    })
}

fn apply_authentication(
    headers: &mut HeaderMap,
    mode: AuthenticationMode,
    profile: &ProviderProfile,
) -> Result<(), GatewayError> {
    let (name, value) = match mode {
        AuthenticationMode::ApiKey => {
            if let Some(secret) = profile.api_key_secret() {
                (HeaderName::from_static("x-api-key"), secret)
            } else if let Some(secret) = profile.bearer_secret() {
                let value = HeaderValue::from_str(&format!("Bearer {secret}"))
                    .map_err(|_| GatewayError::InvalidConfiguration)?;
                headers.insert(header::AUTHORIZATION, value);
                return Ok(());
            } else {
                return Err(GatewayError::InvalidConfiguration);
            }
        }
        AuthenticationMode::GoogleApiKey => (
            HeaderName::from_static("x-goog-api-key"),
            profile
                .api_key_secret()
                .ok_or(GatewayError::InvalidConfiguration)?,
        ),
        AuthenticationMode::Bearer => {
            let value = HeaderValue::from_str(&format!(
                "Bearer {}",
                profile
                    .bearer_secret()
                    .ok_or(GatewayError::InvalidConfiguration)?
            ))
            .map_err(|_| GatewayError::InvalidConfiguration)?;
            headers.insert(header::AUTHORIZATION, value);
            return Ok(());
        }
        AuthenticationMode::OptionalBearer => {
            if let Some(secret) = profile.bearer_secret() {
                let value = HeaderValue::from_str(&format!("Bearer {secret}"))
                    .map_err(|_| GatewayError::InvalidConfiguration)?;
                headers.insert(header::AUTHORIZATION, value);
            }
            return Ok(());
        }
    };
    headers.insert(
        name,
        HeaderValue::from_str(value).map_err(|_| GatewayError::InvalidConfiguration)?,
    );
    Ok(())
}

#[allow(dead_code)]
fn standard_cost(usage: &CanonicalUsage, cost_metadata: &Value) -> Result<u64, GatewayError> {
    cost_for_usage(
        cost_metadata,
        &crate::ledger::TokenUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            cache_write_input_tokens: usage.cache_write_input_tokens,
            reasoning_tokens: usage.reasoning_tokens,
        },
    )
    .map_err(|_| GatewayError::InvalidConfiguration)
}

struct AnthropicProviderAdapter;
struct OpenAiProviderAdapter;
pub struct GeminiProvider;
pub struct XaiProvider;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalRuntime {
    Vllm,
    Ollama,
}

pub struct LocalProvider {
    runtime: LocalRuntime,
}

impl ProviderAdapter for AnthropicProviderAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    fn dialect(&self) -> ProviderDialect {
        ProviderDialect::Anthropic
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: true,
            structured_output: true,
            reasoning: true,
            usage: UsageAvailability::Required,
        }
    }

    fn health(
        &self,
        _profile: &ProviderProfile,
        _provider_model_id: &str,
    ) -> Result<ProviderHealthProbe, GatewayError> {
        Ok(ProviderHealthProbe {
            method: Method::GET,
            path_and_query: "/v1/models".to_owned(),
            authentication: self.authentication(),
        })
    }

    fn send(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError> {
        prepare_request(self, plan, body, profile, input)
    }

    fn stream(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError> {
        prepare_request(self, plan, body, profile, input)
    }

    fn estimate_cost(
        &self,
        usage: &CanonicalUsage,
        cost_metadata: &Value,
    ) -> Result<u64, GatewayError> {
        standard_cost(usage, cost_metadata)
    }

    fn usage(&self, response: &Value) -> Result<Option<CanonicalUsage>, GatewayError> {
        openai_compatible_usage(response)
    }

    fn target_operation(&self, operation: CanonicalOperation) -> Option<GatewayOperation> {
        Some(match operation {
            CanonicalOperation::Generate => GatewayOperation::AnthropicMessages,
            CanonicalOperation::CountTokens => GatewayOperation::AnthropicCountTokens,
        })
    }

    fn default_path(&self, operation: GatewayOperation) -> Option<&'static str> {
        match operation {
            GatewayOperation::AnthropicMessages => Some("/v1/messages"),
            GatewayOperation::AnthropicCountTokens => Some("/v1/messages/count_tokens"),
            _ => None,
        }
    }

    fn authentication(&self) -> AuthenticationMode {
        AuthenticationMode::ApiKey
    }

    fn allows_client_header(&self, name: &HeaderName) -> bool {
        let text = name.as_str();
        name == header::CONTENT_TYPE
            || name == header::ACCEPT
            || text.starts_with("anthropic-")
            || matches!(
                text,
                "x-claude-code-session-id" | "x-stainless-retry-count" | "x-cc-original-request-id"
            )
    }

    fn encode_canonical(
        &self,
        request: &CanonicalRequest,
        target_operation: GatewayOperation,
        provider_model_id: &str,
    ) -> Result<Bytes, GatewayError> {
        if target_operation != GatewayOperation::AnthropicMessages {
            return Err(GatewayError::UnsupportedRequest);
        }
        encode_anthropic_request(request, provider_model_id)
    }
}

impl ProviderAdapter for OpenAiProviderAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAi
    }

    fn dialect(&self) -> ProviderDialect {
        ProviderDialect::OpenAi
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: true,
            structured_output: true,
            reasoning: true,
            usage: UsageAvailability::Required,
        }
    }

    fn health(
        &self,
        _profile: &ProviderProfile,
        _provider_model_id: &str,
    ) -> Result<ProviderHealthProbe, GatewayError> {
        Ok(ProviderHealthProbe {
            method: Method::GET,
            path_and_query: "/v1/models".to_owned(),
            authentication: self.authentication(),
        })
    }

    fn send(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError> {
        prepare_request(self, plan, body, profile, input)
    }

    fn stream(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError> {
        prepare_request(self, plan, body, profile, input)
    }

    fn estimate_cost(
        &self,
        usage: &CanonicalUsage,
        cost_metadata: &Value,
    ) -> Result<u64, GatewayError> {
        standard_cost(usage, cost_metadata)
    }

    fn usage(&self, response: &Value) -> Result<Option<CanonicalUsage>, GatewayError> {
        openai_compatible_usage(response)
    }

    fn target_operation(&self, operation: CanonicalOperation) -> Option<GatewayOperation> {
        match operation {
            CanonicalOperation::Generate => Some(GatewayOperation::OpenAiResponses),
            CanonicalOperation::CountTokens => None,
        }
    }

    fn default_path(&self, operation: GatewayOperation) -> Option<&'static str> {
        match operation {
            GatewayOperation::OpenAiResponses => Some("/v1/responses"),
            GatewayOperation::OpenAiChatCompletions => Some("/v1/chat/completions"),
            _ => None,
        }
    }

    fn authentication(&self) -> AuthenticationMode {
        AuthenticationMode::Bearer
    }

    fn allows_client_header(&self, name: &HeaderName) -> bool {
        let text = name.as_str();
        name == header::CONTENT_TYPE
            || name == header::ACCEPT
            || text == "openai-beta"
            || text == "originator"
            || text == "session_id"
            || text.starts_with("x-stainless-")
    }

    fn encode_canonical(
        &self,
        request: &CanonicalRequest,
        target_operation: GatewayOperation,
        provider_model_id: &str,
    ) -> Result<Bytes, GatewayError> {
        if target_operation != GatewayOperation::OpenAiResponses {
            return Err(GatewayError::UnsupportedRequest);
        }
        encode_openai_request(request, provider_model_id)
    }
}

impl ProviderAdapter for GeminiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn dialect(&self) -> ProviderDialect {
        ProviderDialect::Gemini
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: false,
            structured_output: true,
            reasoning: false,
            usage: UsageAvailability::Required,
        }
    }

    fn health(
        &self,
        _profile: &ProviderProfile,
        provider_model_id: &str,
    ) -> Result<ProviderHealthProbe, GatewayError> {
        Ok(ProviderHealthProbe {
            method: Method::GET,
            path_and_query: format!("/v1beta/models/{provider_model_id}"),
            authentication: self.authentication(),
        })
    }

    fn send(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError> {
        prepare_request(self, plan, body, profile, input)
    }

    fn stream(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError> {
        prepare_request(self, plan, body, profile, input)
    }

    fn estimate_cost(
        &self,
        usage: &CanonicalUsage,
        cost_metadata: &Value,
    ) -> Result<u64, GatewayError> {
        standard_cost(usage, cost_metadata)
    }

    fn usage(&self, response: &Value) -> Result<Option<CanonicalUsage>, GatewayError> {
        gemini_wire::usage(response)
    }

    fn target_operation(&self, operation: CanonicalOperation) -> Option<GatewayOperation> {
        (operation == CanonicalOperation::Generate)
            .then_some(GatewayOperation::GeminiGenerateContent)
    }

    fn default_path(&self, _operation: GatewayOperation) -> Option<&'static str> {
        None
    }

    fn target_path(
        &self,
        operation: GatewayOperation,
        provider_model_id: &str,
        stream: bool,
    ) -> Result<String, GatewayError> {
        if operation != GatewayOperation::GeminiGenerateContent || provider_model_id.is_empty() {
            return Err(GatewayError::InvalidConfiguration);
        }
        let method = if stream {
            concat!("stream", "Generate", "Content?alt=sse")
        } else {
            concat!("generate", "Content")
        };
        Ok(format!("/v1beta/models/{provider_model_id}:{method}"))
    }

    fn authentication(&self) -> AuthenticationMode {
        AuthenticationMode::GoogleApiKey
    }

    fn allows_client_header(&self, name: &HeaderName) -> bool {
        name == header::CONTENT_TYPE || name == header::ACCEPT
    }

    fn encode_canonical(
        &self,
        request: &CanonicalRequest,
        target_operation: GatewayOperation,
        provider_model_id: &str,
    ) -> Result<Bytes, GatewayError> {
        if target_operation != GatewayOperation::GeminiGenerateContent {
            return Err(GatewayError::UnsupportedRequest);
        }
        if provider_model_id.is_empty() {
            return Err(GatewayError::InvalidConfiguration);
        }
        gemini_wire::encode(request).map(Bytes::from)
    }
}

impl ProviderAdapter for XaiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Xai
    }

    fn dialect(&self) -> ProviderDialect {
        ProviderDialect::OpenAi
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: true,
            structured_output: true,
            reasoning: true,
            usage: UsageAvailability::Required,
        }
    }

    fn health(
        &self,
        _profile: &ProviderProfile,
        _provider_model_id: &str,
    ) -> Result<ProviderHealthProbe, GatewayError> {
        Ok(ProviderHealthProbe {
            method: Method::GET,
            path_and_query: "/v1/models".to_owned(),
            authentication: self.authentication(),
        })
    }

    fn send(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError> {
        prepare_request(self, plan, body, profile, input)
    }

    fn stream(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError> {
        prepare_request(self, plan, body, profile, input)
    }

    fn estimate_cost(
        &self,
        usage: &CanonicalUsage,
        cost_metadata: &Value,
    ) -> Result<u64, GatewayError> {
        standard_cost(usage, cost_metadata)
    }

    fn usage(&self, response: &Value) -> Result<Option<CanonicalUsage>, GatewayError> {
        openai_compatible_usage(response)
    }

    fn target_operation(&self, operation: CanonicalOperation) -> Option<GatewayOperation> {
        (operation == CanonicalOperation::Generate).then_some(GatewayOperation::OpenAiResponses)
    }

    fn default_path(&self, operation: GatewayOperation) -> Option<&'static str> {
        match operation {
            GatewayOperation::OpenAiResponses => Some("/v1/responses"),
            GatewayOperation::OpenAiChatCompletions => Some("/v1/chat/completions"),
            _ => None,
        }
    }

    fn authentication(&self) -> AuthenticationMode {
        AuthenticationMode::Bearer
    }

    fn allows_client_header(&self, name: &HeaderName) -> bool {
        name == header::CONTENT_TYPE || name == header::ACCEPT
    }

    fn encode_canonical(
        &self,
        request: &CanonicalRequest,
        target_operation: GatewayOperation,
        provider_model_id: &str,
    ) -> Result<Bytes, GatewayError> {
        if target_operation != GatewayOperation::OpenAiResponses {
            return Err(GatewayError::UnsupportedRequest);
        }
        encode_openai_request(request, provider_model_id)
    }
}

impl ProviderAdapter for LocalProvider {
    fn kind(&self) -> ProviderKind {
        match self.runtime {
            LocalRuntime::Vllm => ProviderKind::Vllm,
            LocalRuntime::Ollama => ProviderKind::Ollama,
        }
    }

    fn dialect(&self) -> ProviderDialect {
        ProviderDialect::OpenAi
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: true,
            structured_output: true,
            reasoning: true,
            usage: UsageAvailability::Optional,
        }
    }

    fn health(
        &self,
        _profile: &ProviderProfile,
        _provider_model_id: &str,
    ) -> Result<ProviderHealthProbe, GatewayError> {
        Ok(ProviderHealthProbe {
            method: Method::GET,
            path_and_query: match self.runtime {
                LocalRuntime::Vllm => "/health",
                LocalRuntime::Ollama => "/api/version",
            }
            .to_owned(),
            authentication: self.authentication(),
        })
    }

    fn send(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError> {
        prepare_request(self, plan, body, profile, input)
    }

    fn stream(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError> {
        prepare_request(self, plan, body, profile, input)
    }

    fn estimate_cost(
        &self,
        usage: &CanonicalUsage,
        cost_metadata: &Value,
    ) -> Result<u64, GatewayError> {
        standard_cost(usage, cost_metadata)
    }

    fn usage(&self, response: &Value) -> Result<Option<CanonicalUsage>, GatewayError> {
        openai_compatible_usage(response)
    }

    fn target_operation(&self, operation: CanonicalOperation) -> Option<GatewayOperation> {
        (operation == CanonicalOperation::Generate).then_some(GatewayOperation::OpenAiResponses)
    }

    fn default_path(&self, operation: GatewayOperation) -> Option<&'static str> {
        match operation {
            GatewayOperation::OpenAiResponses => Some("/v1/responses"),
            GatewayOperation::OpenAiChatCompletions => Some("/v1/chat/completions"),
            _ => None,
        }
    }

    fn authentication(&self) -> AuthenticationMode {
        AuthenticationMode::OptionalBearer
    }

    fn allows_client_header(&self, name: &HeaderName) -> bool {
        name == header::CONTENT_TYPE || name == header::ACCEPT
    }

    fn encode_canonical(
        &self,
        request: &CanonicalRequest,
        target_operation: GatewayOperation,
        provider_model_id: &str,
    ) -> Result<Bytes, GatewayError> {
        if target_operation != GatewayOperation::OpenAiResponses {
            return Err(GatewayError::UnsupportedRequest);
        }
        encode_openai_request(request, provider_model_id)
    }
}

fn openai_compatible_usage(response: &Value) -> Result<Option<CanonicalUsage>, GatewayError> {
    let Some(usage) = response.get("usage").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    if !usage.is_object() {
        return Err(GatewayError::UpstreamProtocol);
    }
    normalize_openai_usage(crate::ledger::TokenUsage::from_value(usage)).map(Some)
}

pub(crate) fn normalize_openai_usage(
    usage: crate::ledger::TokenUsage,
) -> Result<CanonicalUsage, GatewayError> {
    let detailed_input = usage
        .cached_input_tokens
        .checked_add(usage.cache_write_input_tokens)
        .ok_or(GatewayError::UpstreamProtocol)?;
    Ok(CanonicalUsage {
        input_tokens: usage
            .input_tokens
            .checked_sub(detailed_input)
            .ok_or(GatewayError::UpstreamProtocol)?,
        output_tokens: usage
            .output_tokens
            .checked_sub(usage.reasoning_tokens)
            .ok_or(GatewayError::UpstreamProtocol)?,
        cached_input_tokens: usage.cached_input_tokens,
        cache_write_input_tokens: usage.cache_write_input_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        ..CanonicalUsage::default()
    })
}

fn encode_openai_request(
    request: &CanonicalRequest,
    provider_model_id: &str,
) -> Result<Bytes, GatewayError> {
    let mut input = Vec::new();
    for message in &request.messages {
        let role = match message.role {
            CanonicalRole::System => "system",
            CanonicalRole::User | CanonicalRole::Tool => "user",
            CanonicalRole::Assistant => "assistant",
            CanonicalRole::Developer | CanonicalRole::Other(_) => {
                return Err(GatewayError::UnsupportedRequest);
            }
        };
        let mut text_content = Vec::new();
        for content in &message.content {
            match content {
                CanonicalContent::Text { text, .. } => text_content.push(json!({
                    "type": if role == "assistant" { "output_text" } else { "input_text" },
                    "text": text
                })),
                CanonicalContent::ToolCall(call) => {
                    push_openai_text(&mut input, role, &mut text_content);
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": arguments_json(&call.arguments)?.to_string()
                    }));
                }
                CanonicalContent::ToolResult {
                    tool_call_id,
                    content,
                    ..
                } => {
                    push_openai_text(&mut input, role, &mut text_content);
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_call_id,
                        "output": value_text(content)
                    }));
                }
                _ => return Err(GatewayError::UnsupportedRequest),
            }
        }
        push_openai_text(&mut input, role, &mut text_content);
    }
    let mut document = Map::from_iter([
        (
            "model".to_owned(),
            Value::String(provider_model_id.to_owned()),
        ),
        ("input".to_owned(), Value::Array(input)),
        ("stream".to_owned(), Value::Bool(request.stream)),
    ]);
    if let Some(limit) = request.max_output_tokens {
        document.insert("max_output_tokens".to_owned(), Value::from(limit));
    }
    if !request.tools.is_empty() {
        document.insert(
            "tools".to_owned(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        let mut value = json!({
                            "type": "function",
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema
                        });
                        if let Some(strict) = tool.strict {
                            value["strict"] = Value::Bool(strict);
                        }
                        value
                    })
                    .collect(),
            ),
        );
    }
    if let Some(choice) = &request.tool_choice {
        document.insert("tool_choice".to_owned(), openai_tool_choice(choice));
    }
    if let Some(output) = &request.structured_output {
        document.insert(
            "text".to_owned(),
            json!({"format": {
                "type": "json_schema",
                "name": output.name.as_deref().unwrap_or("response"),
                "description": output.description,
                "schema": output.schema,
                "strict": true
            }}),
        );
    }
    serde_json::to_vec(&Value::Object(document))
        .map(Bytes::from)
        .map_err(GatewayError::InvalidRequest)
}

fn push_openai_text(input: &mut Vec<Value>, role: &str, content: &mut Vec<Value>) {
    if !content.is_empty() {
        input.push(json!({
            "type": "message",
            "role": role,
            "content": std::mem::take(content)
        }));
    }
}

fn encode_anthropic_request(
    request: &CanonicalRequest,
    provider_model_id: &str,
) -> Result<Bytes, GatewayError> {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in &request.messages {
        let mut content = Vec::new();
        for item in &message.content {
            match item {
                CanonicalContent::Text { text, .. } => {
                    content.push(json!({"type": "text", "text": text}));
                }
                CanonicalContent::ToolCall(call) => content.push(json!({
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": arguments_json(&call.arguments)?
                })),
                CanonicalContent::ToolResult {
                    tool_call_id,
                    content: result,
                    ..
                } => content.push(json!({
                    "type": "tool_result",
                    "tool_use_id": tool_call_id,
                    "content": result
                })),
                _ => return Err(GatewayError::UnsupportedRequest),
            }
        }
        match message.role {
            CanonicalRole::System => system.extend(content),
            CanonicalRole::User | CanonicalRole::Tool => {
                messages.push(json!({"role": "user", "content": content}));
            }
            CanonicalRole::Assistant => {
                messages.push(json!({"role": "assistant", "content": content}));
            }
            CanonicalRole::Developer | CanonicalRole::Other(_) => {
                return Err(GatewayError::UnsupportedRequest);
            }
        }
    }
    let max_tokens = request
        .max_output_tokens
        .ok_or(GatewayError::UnsupportedRequest)?;
    let mut document = Map::from_iter([
        (
            "model".to_owned(),
            Value::String(provider_model_id.to_owned()),
        ),
        ("messages".to_owned(), Value::Array(messages)),
        ("max_tokens".to_owned(), Value::from(max_tokens)),
        ("stream".to_owned(), Value::Bool(request.stream)),
    ]);
    if !system.is_empty() {
        document.insert("system".to_owned(), Value::Array(system));
    }
    if !request.tools.is_empty() {
        document.insert(
            "tools".to_owned(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        let mut value = json!({
                            "name": tool.name,
                            "description": tool.description,
                            "input_schema": tool.input_schema
                        });
                        if let Some(strict) = tool.strict {
                            value["strict"] = Value::Bool(strict);
                        }
                        value
                    })
                    .collect(),
            ),
        );
    }
    if let Some(choice) = &request.tool_choice {
        document.insert("tool_choice".to_owned(), anthropic_tool_choice(choice));
    }
    if let Some(output) = &request.structured_output {
        document.insert(
            "output_config".to_owned(),
            json!({"format": {"type": "json_schema", "schema": output.schema}}),
        );
    }
    serde_json::to_vec(&Value::Object(document))
        .map(Bytes::from)
        .map_err(GatewayError::InvalidRequest)
}

fn arguments_json(arguments: &CanonicalToolArguments) -> Result<Value, GatewayError> {
    match arguments {
        CanonicalToolArguments::Complete(Value::String(value)) => {
            serde_json::from_str(value).map_err(|_| GatewayError::UnsupportedRequest)
        }
        CanonicalToolArguments::Complete(value) if value.is_object() => Ok(value.clone()),
        _ => Err(GatewayError::UnsupportedRequest),
    }
}

fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn openai_tool_choice(choice: &CanonicalToolChoice) -> Value {
    match choice {
        CanonicalToolChoice::Auto => json!("auto"),
        CanonicalToolChoice::Required => json!("required"),
        CanonicalToolChoice::None => json!("none"),
        CanonicalToolChoice::Named(name) => json!({"type": "function", "name": name}),
    }
}

fn anthropic_tool_choice(choice: &CanonicalToolChoice) -> Value {
    match choice {
        CanonicalToolChoice::Auto => json!({"type": "auto"}),
        CanonicalToolChoice::Required => json!({"type": "any"}),
        CanonicalToolChoice::None => json!({"type": "none"}),
        CanonicalToolChoice::Named(name) => json!({"type": "tool", "name": name}),
    }
}

static ANTHROPIC: AnthropicProviderAdapter = AnthropicProviderAdapter;
static OPENAI: OpenAiProviderAdapter = OpenAiProviderAdapter;
static GEMINI: GeminiProvider = GeminiProvider;
static XAI: XaiProvider = XaiProvider;
static VLLM: LocalProvider = LocalProvider {
    runtime: LocalRuntime::Vllm,
};
static OLLAMA: LocalProvider = LocalProvider {
    runtime: LocalRuntime::Ollama,
};

#[cfg(test)]
pub fn for_dialect(dialect: ProviderDialect) -> &'static dyn ProviderAdapter {
    match dialect {
        ProviderDialect::Anthropic => &ANTHROPIC,
        ProviderDialect::OpenAi => &OPENAI,
        ProviderDialect::Gemini => &GEMINI,
    }
}

pub fn for_kind(kind: ProviderKind) -> Result<&'static dyn ProviderAdapter, GatewayError> {
    let adapter: &'static dyn ProviderAdapter = match kind {
        ProviderKind::Anthropic => &ANTHROPIC,
        ProviderKind::OpenAi => &OPENAI,
        ProviderKind::Gemini => &GEMINI,
        ProviderKind::Xai => &XAI,
        ProviderKind::Vllm => &VLLM,
        ProviderKind::Ollama => &OLLAMA,
    };
    debug_assert_eq!(adapter.kind(), kind);
    Ok(adapter)
}

pub fn usage_for(
    kind: ProviderKind,
    response: &[u8],
) -> Result<Option<CanonicalUsage>, GatewayError> {
    let response = serde_json::from_slice(response).map_err(|_| GatewayError::UpstreamProtocol)?;
    let adapter = for_kind(kind)?;
    let usage = adapter.usage(&response)?;
    if usage.is_none() && adapter.capabilities().usage == UsageAvailability::Required {
        return Err(GatewayError::UpstreamProtocol);
    }
    Ok(usage)
}

#[tracing::instrument(
    name = "provider.attempt",
    skip_all,
    fields(gen_ai.system = %provider_key, gen_ai.request.model = %provider_model_id)
)]
pub async fn send(
    client: &reqwest::Client,
    base_url: &str,
    request: PreparedProviderRequest,
    provider_key: &str,
    provider_model_id: &str,
) -> Result<reqwest::Response, GatewayError> {
    Ok(client
        .post(format!("{}{}", base_url, request.path_and_query()))
        .headers(request.headers)
        .body(request.body)
        .send()
        .await?)
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::Bytes, http::HeaderMap};
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        AuthenticationMode, NativePayload, UsageAvailability, apply_authentication, for_dialect,
        for_kind,
    };
    use crate::{
        adapters::{ProviderRequestInput, protocol},
        canonical::{CanonicalUsage, PrincipalRef, RequestEnrichment},
        model_registry::{GatewayOperation, ProviderDialect, ProviderKind, ProviderProfile},
    };

    #[test]
    fn unchanged_model_preserves_every_request_byte() {
        let body =
            br#"{ "model": "public", "vendor": {"number": 123456789012345678901234567890} }"#;
        let document = serde_json::from_slice(body).unwrap();
        let encoded = for_dialect(ProviderDialect::OpenAi)
            .encode_native(NativePayload::new(body, &document), "public")
            .unwrap();
        assert_eq!(encoded, Bytes::from_static(body));
    }

    #[test]
    fn api_key_adapter_uses_bearer_when_profile_has_only_bearer_secret() {
        let profile = profile(ProviderKind::Anthropic, true);
        let mut headers = HeaderMap::new();

        apply_authentication(&mut headers, AuthenticationMode::ApiKey, &profile).unwrap();

        assert_eq!(headers.get("authorization").unwrap(), "Bearer secret");
        assert!(headers.get("x-api-key").is_none());
    }

    #[test]
    fn anthropic_client_bearer_is_forwarded_with_oauth_headers() {
        let adapter = for_kind(ProviderKind::Anthropic).unwrap();
        let profile = ProviderProfile::client_bearer(
            "env://ANTHROPIC_CLIENT_OAUTH",
            "anthropic-subscription",
            "https://example.test",
            vec![GatewayOperation::AnthropicMessages],
            vec![Uuid::nil()],
        );
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer client-oauth".parse().unwrap());
        headers.insert("anthropic-beta", "claude-code-20250219".parse().unwrap());
        let input = ProviderRequestInput {
            source_path_and_query: "/v1/messages",
            client_headers: &headers,
            idempotency_key: None,
        };
        let plan = adapter
            .plan_for_target(
                GatewayOperation::AnthropicMessages,
                GatewayOperation::AnthropicMessages,
                "provider-model",
                false,
                &input,
            )
            .unwrap();

        let prepared = adapter
            .send(plan, Bytes::from_static(b"{}"), &profile, input)
            .unwrap();

        assert_eq!(
            prepared.headers().get("authorization").unwrap(),
            "Bearer client-oauth"
        );
        assert_eq!(
            prepared
                .headers()
                .get("anthropic-dangerous-direct-browser-access")
                .unwrap(),
            "true"
        );
        assert_eq!(
            prepared.headers().get("anthropic-beta").unwrap(),
            "claude-code-20250219,oauth-2025-04-20"
        );
    }

    #[test]
    fn anthropic_preserves_claude_code_retry_identity_headers() {
        let adapter = for_kind(ProviderKind::Anthropic).unwrap();
        let profile = profile(ProviderKind::Anthropic, true);
        let mut headers = HeaderMap::new();
        headers.insert("x-claude-code-session-id", "session-one".parse().unwrap());
        headers.insert("x-stainless-retry-count", "2".parse().unwrap());
        headers.insert("x-cc-original-request-id", "request-one".parse().unwrap());
        headers.insert("x-untrusted-client-metadata", "blocked".parse().unwrap());
        let input = ProviderRequestInput {
            source_path_and_query: "/v1/messages",
            client_headers: &headers,
            idempotency_key: None,
        };
        let plan = adapter
            .plan_for_target(
                GatewayOperation::AnthropicMessages,
                GatewayOperation::AnthropicMessages,
                "provider-model",
                true,
                &input,
            )
            .unwrap();

        let prepared = adapter
            .stream(plan, Bytes::from_static(b"{}"), &profile, input)
            .unwrap();

        assert_eq!(
            prepared.headers().get("x-claude-code-session-id").unwrap(),
            "session-one"
        );
        assert_eq!(
            prepared.headers().get("x-stainless-retry-count").unwrap(),
            "2"
        );
        assert_eq!(
            prepared.headers().get("x-cc-original-request-id").unwrap(),
            "request-one"
        );
        assert!(
            prepared
                .headers()
                .get("x-untrusted-client-metadata")
                .is_none()
        );
    }

    #[test]
    fn model_rewrite_preserves_every_other_json_value() {
        let body = br#"{"model":"public","vendor":{"number":123456789012345678901234567890,"nested":{"enabled":true}}}"#;
        let document = serde_json::from_slice(body).unwrap();
        let encoded = for_dialect(ProviderDialect::OpenAi)
            .encode_native(NativePayload::new(body, &document), "provider-model")
            .unwrap();
        let actual: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            actual,
            json!({
                "model": "provider-model",
                "vendor": {
                    "number": 123456789012345678901234567890_u128,
                    "nested": {"enabled": true}
                }
            })
        );
    }

    #[test]
    fn gemini_provider_contract_maps_request_stream_usage_and_cost() {
        let adapter = for_kind(ProviderKind::Gemini).unwrap();
        let profile = profile(ProviderKind::Gemini, true);
        let request = canonical_request(true);
        let headers = HeaderMap::new();
        let plan = adapter
            .plan_for_target(
                GatewayOperation::GeminiGenerateContent,
                GatewayOperation::OpenAiResponses,
                "gemini-2.5-flash",
                true,
                &input(&headers),
            )
            .unwrap();
        assert_eq!(
            plan.path_and_query,
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        let body = adapter
            .encode_canonical(
                &request,
                GatewayOperation::GeminiGenerateContent,
                "gemini-2.5-flash",
            )
            .unwrap();
        let prepared = adapter
            .stream(plan, body, &profile, input(&headers))
            .unwrap();
        assert_eq!(prepared.headers().get("x-goog-api-key").unwrap(), "secret");
        let body: serde_json::Value = serde_json::from_slice(prepared.body()).unwrap();
        assert_eq!(body["contents"][0]["parts"][0]["text"], "hello");
        assert!(!adapter.capabilities().tools);
        assert!(!adapter.capabilities().reasoning);
        let usage = adapter
            .usage(&json!({"usageMetadata": {
                "promptTokenCount": 11,
                "cachedContentTokenCount": 3,
                "candidatesTokenCount": 5,
                "thoughtsTokenCount": 2
            }}))
            .unwrap()
            .unwrap();
        assert_eq!(
            usage,
            CanonicalUsage {
                input_tokens: 8,
                cached_input_tokens: 3,
                output_tokens: 5,
                reasoning_tokens: 2,
                ..CanonicalUsage::default()
            }
        );
        assert_eq!(
            adapter.estimate_cost(&priced_usage(), &pricing()).unwrap(),
            3
        );
        let disjoint = CanonicalUsage {
            input_tokens: 750_000,
            cached_input_tokens: 250_000,
            ..CanonicalUsage::default()
        };
        let tiered_pricing = json!({
            "schema": "token_v1",
            "currency": "USD",
            "input_microusd_per_million_tokens": 4,
            "output_microusd_per_million_tokens": 0,
            "cache_read_input_microusd_per_million_tokens": 1
        });
        assert_eq!(
            adapter.estimate_cost(&disjoint, &tiered_pricing).unwrap(),
            4
        );
        assert!(
            adapter
                .usage(&json!({"usageMetadata": {
                    "promptTokenCount": 1,
                    "cachedContentTokenCount": 2,
                    "candidatesTokenCount": 1
                }}))
                .is_err()
        );
    }

    #[test]
    fn xai_provider_contract_is_explicit_openai_compatibility() {
        let adapter = for_kind(ProviderKind::Xai).unwrap();
        let profile = profile(ProviderKind::Xai, true);
        let headers = HeaderMap::new();
        let plan = adapter
            .plan_for_target(
                GatewayOperation::OpenAiResponses,
                GatewayOperation::AnthropicMessages,
                "grok-4",
                false,
                &input(&headers),
            )
            .unwrap();
        let prepared = adapter
            .send(plan, Bytes::from_static(b"{}"), &profile, input(&headers))
            .unwrap();
        assert_eq!(prepared.path_and_query(), "/v1/responses");
        assert_eq!(
            prepared.headers().get("authorization").unwrap(),
            "Bearer secret"
        );
        let usage = adapter
            .usage(&json!({"usage": {
                "input_tokens": 9,
                "output_tokens": 4,
                "input_tokens_details": {"cached_tokens": 6},
                "output_tokens_details": {"reasoning_tokens": 2}
            }}))
            .unwrap()
            .unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (3, 2));
        assert_eq!((usage.cached_input_tokens, usage.reasoning_tokens), (6, 2));
        assert_eq!(adapter.capabilities().usage, UsageAvailability::Required);
        assert_eq!(
            adapter.estimate_cost(&priced_usage(), &pricing()).unwrap(),
            3
        );
    }

    #[test]
    fn local_provider_contract_distinguishes_vllm_and_ollama() {
        let vllm = for_kind(ProviderKind::Vllm).unwrap();
        let ollama = for_kind(ProviderKind::Ollama).unwrap();
        let vllm_profile = profile(ProviderKind::Vllm, false);
        let headers = HeaderMap::new();
        assert_eq!(
            vllm.health(&vllm_profile, "model").unwrap().path_and_query,
            "/health"
        );
        assert_eq!(
            ollama
                .health(&profile(ProviderKind::Ollama, false), "model")
                .unwrap()
                .path_and_query,
            "/api/version"
        );
        let plan = vllm
            .plan_for_target(
                GatewayOperation::OpenAiResponses,
                GatewayOperation::AnthropicMessages,
                "model",
                true,
                &input(&headers),
            )
            .unwrap();
        let prepared = vllm
            .stream(
                plan,
                Bytes::from_static(b"{}"),
                &vllm_profile,
                input(&headers),
            )
            .unwrap();
        assert!(prepared.headers().get("authorization").is_none());
        assert_eq!(vllm.capabilities().usage, UsageAvailability::Optional);
        assert_eq!(vllm.usage(&json!({})).unwrap(), None);
        assert!(vllm.usage(&json!({"usage": "invalid"})).is_err());
        assert_eq!(
            ollama.estimate_cost(&priced_usage(), &pricing()).unwrap(),
            3
        );
    }

    #[tokio::test]
    async fn every_provider_health_probe_executes_once_and_preserves_error_status() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().fallback(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE }),
            )
            .await
            .unwrap();
        });
        let base_url = format!("http://{address}");
        let client = reqwest::Client::new();
        let gemini = profile(ProviderKind::Gemini, true);
        let xai = profile(ProviderKind::Xai, true);
        let vllm = profile(ProviderKind::Vllm, false);
        let ollama = profile(ProviderKind::Ollama, false);
        assert_eq!(
            for_kind(ProviderKind::Gemini)
                .unwrap()
                .health(&gemini, "model")
                .unwrap()
                .execute(&client, &base_url, &gemini)
                .await
                .unwrap(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            for_kind(ProviderKind::Xai)
                .unwrap()
                .health(&xai, "model")
                .unwrap()
                .execute(&client, &base_url, &xai)
                .await
                .unwrap(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            for_kind(ProviderKind::Vllm)
                .unwrap()
                .health(&vllm, "model")
                .unwrap()
                .execute(&client, &base_url, &vllm)
                .await
                .unwrap(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            for_kind(ProviderKind::Ollama)
                .unwrap()
                .health(&ollama, "model")
                .unwrap()
                .execute(&client, &base_url, &ollama)
                .await
                .unwrap(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        task.abort();
    }

    fn canonical_request(stream: bool) -> crate::canonical::CanonicalRequest {
        protocol::decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from(format!(
                r#"{{"model":"public","input":"hello","stream":{stream},"max_output_tokens":16}}"#
            )),
        )
        .unwrap()
        .enrich(RequestEnrichment {
            gateway_request_id: Uuid::nil(),
            trace_id: None,
            session_id: None,
            principal: PrincipalRef {
                tenant_id: Uuid::nil(),
                credential_id: Uuid::nil(),
                user_id: None,
                device_id: None,
                client_id: None,
                agent_id: None,
            },
            requested_model: "public".to_owned(),
            requested_quality: None,
            workload_class: "interactive".to_owned(),
            urgency: "normal".to_owned(),
            budget_class: "standard".to_owned(),
            deadline: None,
        })
    }

    fn input(headers: &HeaderMap) -> ProviderRequestInput<'_> {
        ProviderRequestInput {
            source_path_and_query: "/v1/responses",
            client_headers: headers,
            idempotency_key: None,
        }
    }

    fn profile(kind: ProviderKind, authenticated: bool) -> ProviderProfile {
        let operations = match kind {
            ProviderKind::Gemini => vec![GatewayOperation::GeminiGenerateContent],
            _ => vec![
                GatewayOperation::OpenAiResponses,
                GatewayOperation::OpenAiChatCompletions,
            ],
        };
        if kind == ProviderKind::Gemini {
            ProviderProfile::api_key_for(
                kind,
                format!("env://{}", kind.as_str()),
                kind.as_str(),
                "https://example.test",
                operations,
                vec![Uuid::nil()],
                "secret",
            )
        } else if authenticated {
            ProviderProfile::bearer_for(
                kind,
                format!("env://{}", kind.as_str()),
                kind.as_str(),
                "https://example.test",
                operations,
                vec![Uuid::nil()],
                "secret",
            )
        } else {
            ProviderProfile::unauthenticated_for(
                kind,
                format!("none://{}", kind.as_str()),
                kind.as_str(),
                "https://example.test",
                operations,
                vec![Uuid::nil()],
            )
        }
    }

    fn priced_usage() -> CanonicalUsage {
        CanonicalUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..CanonicalUsage::default()
        }
    }

    fn pricing() -> serde_json::Value {
        json!({
            "schema": "token_v1",
            "currency": "USD",
            "input_microusd_per_million_tokens": 1,
            "output_microusd_per_million_tokens": 2
        })
    }
}
