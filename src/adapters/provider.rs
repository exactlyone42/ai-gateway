use axum::{
    body::Bytes,
    http::{HeaderMap, HeaderName, HeaderValue, header},
};
use serde_json::Value;

use crate::{
    adapters::ProviderRequestInput,
    canonical::CanonicalRequest,
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

    fn plan(
        &self,
        operation: CanonicalOperation,
        source_operation: GatewayOperation,
        input: &ProviderRequestInput<'_>,
    ) -> Result<ProviderRequestPlan, GatewayError> {
        let target_operation = if source_operation.dialect() == self.dialect() {
            source_operation
        } else {
            self.target_operation(operation)
                .ok_or(GatewayError::UnsupportedRequest)?
        };
        let path_and_query = if target_operation == source_operation {
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
            if self.allows_client_header(name)
                && name != header::AUTHORIZATION
                && name.as_str() != "x-api-key"
            {
                headers.append(name.clone(), value.clone());
            }
        }
        if self.dialect() == ProviderDialect::OpenAi
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
