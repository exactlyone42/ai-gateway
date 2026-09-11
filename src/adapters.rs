pub mod protocol;
pub mod provider;

use axum::http::{HeaderMap, HeaderValue};

use crate::{canonical::CanonicalRequest, error::GatewayError, model_registry::ProviderProfile};

pub struct ProviderRequestInput<'a> {
    pub source_path_and_query: &'a str,
    pub client_headers: &'a HeaderMap,
    pub idempotency_key: Option<HeaderValue>,
}

pub fn prepare_for_provider(
    request: &CanonicalRequest,
    profile: &ProviderProfile,
    provider_model_id: &str,
    input: ProviderRequestInput<'_>,
) -> Result<provider::PreparedProviderRequest, GatewayError> {
    let native = request.native();
    let adapter = provider::for_dialect(profile.target_dialect);
    let plan = adapter.plan(
        request.native().operation.canonical(),
        native.operation,
        &input,
    )?;
    let body = if native.operation == plan.operation {
        adapter.encode_native(
            provider::NativePayload::new(&native.body, &native.document),
            provider_model_id,
        )?
    } else {
        adapter.encode_canonical(request, plan.operation, provider_model_id)?
    };
    adapter.prepare(plan, body, profile, input)
}

#[cfg(test)]
mod tests {
    use axum::{body::Bytes, http::HeaderMap};
    use uuid::Uuid;

    use super::{ProviderRequestInput, prepare_for_provider, protocol};
    use crate::{
        canonical::{PrincipalRef, RequestEnrichment},
        model_registry::{CanonicalOperation, GatewayOperation, ProviderDialect, ProviderProfile},
    };

    const CASES: &[(GatewayOperation, &[u8], &[u8])] = &[
        (
            GatewayOperation::AnthropicMessages,
            br#"{ "model": "public", "messages": [], "max_tokens": 1, "vendor": 7 }"#,
            br#"{"max_tokens":1,"messages":[],"model":"provider-model","vendor":7}"#,
        ),
        (
            GatewayOperation::AnthropicCountTokens,
            br#"{ "model": "public", "messages": [], "vendor": 7 }"#,
            br#"{"messages":[],"model":"provider-model","vendor":7}"#,
        ),
        (
            GatewayOperation::OpenAiResponses,
            br#"{ "model": "public", "input": "hello", "vendor": 7 }"#,
            br#"{"input":"hello","model":"provider-model","vendor":7}"#,
        ),
        (
            GatewayOperation::OpenAiChatCompletions,
            br#"{ "model": "public", "messages": [], "vendor": 7 }"#,
            br#"{"messages":[],"model":"provider-model","vendor":7}"#,
        ),
    ];

    #[test]
    fn all_operations_match_the_legacy_encoder_for_unchanged_and_changed_models() {
        for (operation, body, changed_golden) in CASES {
            let headers = HeaderMap::new();
            let canonical = protocol::decode(*operation, Bytes::copy_from_slice(body))
                .unwrap()
                .enrich(enrichment());
            let profile = profile(operation.dialect());
            let unchanged = prepare_for_provider(
                &canonical,
                &profile,
                "public",
                input(source_path(*operation), &headers),
            )
            .unwrap();
            assert_eq!(unchanged.body().as_ref(), *body, "unchanged {operation:?}");

            let changed = prepare_for_provider(
                &canonical,
                &profile,
                "provider-model",
                input(source_path(*operation), &headers),
            )
            .unwrap();
            assert_eq!(
                changed.body().as_ref(),
                *changed_golden,
                "changed {operation:?}"
            );
        }
    }

    #[test]
    fn target_adapter_selects_operation_uri_and_authentication() {
        let headers = HeaderMap::new();
        let input = input("/v1/messages?source=query", &headers);
        let plan = super::provider::for_dialect(ProviderDialect::OpenAi)
            .plan(
                CanonicalOperation::Generate,
                GatewayOperation::AnthropicMessages,
                &input,
            )
            .unwrap();
        assert_eq!(plan.operation, GatewayOperation::OpenAiResponses);
        assert_eq!(plan.path_and_query, "/v1/responses");
        assert_eq!(
            plan.authentication,
            super::provider::AuthenticationMode::Bearer
        );
    }

    fn input<'a>(path: &'a str, headers: &'a HeaderMap) -> ProviderRequestInput<'a> {
        ProviderRequestInput {
            source_path_and_query: path,
            client_headers: headers,
            idempotency_key: None,
        }
    }

    fn source_path(operation: GatewayOperation) -> &'static str {
        match operation {
            GatewayOperation::AnthropicMessages => "/v1/messages?native=true",
            GatewayOperation::AnthropicCountTokens => "/v1/messages/count_tokens?native=true",
            GatewayOperation::OpenAiResponses => "/v1/responses?native=true",
            GatewayOperation::OpenAiChatCompletions => "/v1/chat/completions?native=true",
        }
    }

    fn profile(dialect: ProviderDialect) -> ProviderProfile {
        let operations = match dialect {
            ProviderDialect::Anthropic => vec![
                GatewayOperation::AnthropicMessages,
                GatewayOperation::AnthropicCountTokens,
            ],
            ProviderDialect::OpenAi => vec![
                GatewayOperation::OpenAiResponses,
                GatewayOperation::OpenAiChatCompletions,
            ],
        };
        match dialect {
            ProviderDialect::Anthropic => ProviderProfile::api_key(
                "env://KEY",
                "provider",
                "https://example.test",
                dialect,
                operations,
                vec![Uuid::nil()],
                "secret",
            ),
            ProviderDialect::OpenAi => ProviderProfile::bearer(
                "env://KEY",
                "provider",
                "https://example.test",
                dialect,
                operations,
                vec![Uuid::nil()],
                "secret",
            ),
        }
    }

    fn enrichment() -> RequestEnrichment {
        RequestEnrichment {
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
        }
    }
}
