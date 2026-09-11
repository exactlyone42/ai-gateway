use axum::{
    body::Bytes,
    http::{HeaderMap, HeaderName, HeaderValue, header},
};
use serde_json::{Map, Value, json};

use crate::{
    adapters::ProviderRequestInput,
    canonical::{
        CanonicalContent, CanonicalRequest, CanonicalRole, CanonicalToolArguments,
        CanonicalToolChoice,
    },
    error::GatewayError,
    logging,
    model_registry::{
        CanonicalOperation, GatewayOperation, ProviderDialect, ProviderProfile, rewrite_model,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticationMode {
    ApiKey,
    Bearer,
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
    fn dialect(&self) -> ProviderDialect;

    fn target_operation(&self, operation: CanonicalOperation) -> Option<GatewayOperation>;

    fn default_path(&self, operation: GatewayOperation) -> Option<&'static str>;

    fn authentication(&self) -> AuthenticationMode;

    fn allows_client_header(&self, name: &HeaderName) -> bool;

    fn plan_for_target(
        &self,
        target_operation: GatewayOperation,
        source_operation: GatewayOperation,
        input: &ProviderRequestInput<'_>,
    ) -> Result<ProviderRequestPlan, GatewayError> {
        if target_operation.dialect() != self.dialect() {
            return Err(GatewayError::InvalidConfiguration);
        }
        let translated = target_operation != source_operation;
        let path_and_query = if !translated {
            input.source_path_and_query.to_owned()
        } else {
            self.default_path(target_operation)
                .ok_or(GatewayError::InvalidConfiguration)?
                .to_owned()
        };
        Ok(ProviderRequestPlan {
            operation: target_operation,
            path_and_query,
            authentication: self.authentication(),
            translated,
        })
    }

    fn prepare(
        &self,
        plan: ProviderRequestPlan,
        body: Bytes,
        profile: &ProviderProfile,
        input: ProviderRequestInput<'_>,
    ) -> Result<PreparedProviderRequest, GatewayError> {
        let mut headers = HeaderMap::new();
        for (name, value) in input.client_headers {
            if (!plan.translated || name == header::CONTENT_TYPE || name == header::ACCEPT)
                && self.allows_client_header(name)
                && name != header::AUTHORIZATION
                && name.as_str() != "x-api-key"
            {
                headers.append(name.clone(), value.clone());
            }
        }
        if plan.translated || !headers.contains_key(header::CONTENT_TYPE) {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
        }
        if self.dialect() == ProviderDialect::Anthropic && plan.translated {
            headers.insert(
                HeaderName::from_static("anthropic-version"),
                HeaderValue::from_static("2023-06-01"),
            );
        }
        if self.dialect() == ProviderDialect::OpenAi
            && !plan.translated
            && let Some(value) = input.idempotency_key
        {
            headers.insert(HeaderName::from_static("idempotency-key"), value);
        }
        logging::inject_context(&mut headers);
        match plan.authentication {
            AuthenticationMode::ApiKey => {
                let value = HeaderValue::from_str(
                    profile
                        .api_key_secret()
                        .ok_or(GatewayError::InvalidConfiguration)?,
                )
                .map_err(|_| GatewayError::InvalidConfiguration)?;
                headers.insert(HeaderName::from_static("x-api-key"), value);
            }
            AuthenticationMode::Bearer => {
                let value = HeaderValue::from_str(&format!(
                    "Bearer {}",
                    profile
                        .bearer_secret()
                        .ok_or(GatewayError::InvalidConfiguration)?
                ))
                .map_err(|_| GatewayError::InvalidConfiguration)?;
                headers.insert(header::AUTHORIZATION, value);
            }
        }
        Ok(PreparedProviderRequest {
            plan,
            headers,
            body,
        })
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

struct AnthropicProviderAdapter;
struct OpenAiProviderAdapter;

impl ProviderAdapter for AnthropicProviderAdapter {
    fn dialect(&self) -> ProviderDialect {
        ProviderDialect::Anthropic
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
        name == header::CONTENT_TYPE
            || name == header::ACCEPT
            || name.as_str().starts_with("anthropic-")
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
    fn dialect(&self) -> ProviderDialect {
        ProviderDialect::OpenAi
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

pub fn for_dialect(dialect: ProviderDialect) -> &'static dyn ProviderAdapter {
    match dialect {
        ProviderDialect::Anthropic => &ANTHROPIC,
        ProviderDialect::OpenAi => &OPENAI,
    }
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
    use axum::body::Bytes;
    use serde_json::json;

    use super::{NativePayload, for_dialect};
    use crate::model_registry::ProviderDialect;

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
}
