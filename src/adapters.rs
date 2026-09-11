pub mod protocol;
pub mod provider;
pub mod response;
pub mod stream;

use axum::http::{HeaderMap, HeaderValue};

use serde_json::Value;

use crate::{
    canonical::{
        CanonicalContent, CanonicalRequest, CanonicalRole, CanonicalStructuredOutputKind,
        CanonicalToolArguments, ProviderOptions,
    },
    error::GatewayError,
    model_registry::{GatewayOperation, ModelSelection, ProviderDialect, ProviderProfile},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedTranslationPlan {
    pub source_operation: GatewayOperation,
    pub target_operation: GatewayOperation,
    pub target_dialect: ProviderDialect,
    pub translated: bool,
}

pub struct ProviderRequestInput<'a> {
    pub source_path_and_query: &'a str,
    pub client_headers: &'a HeaderMap,
    pub idempotency_key: Option<HeaderValue>,
}

pub fn validate_for_provider(
    request: &CanonicalRequest,
    selection: &ModelSelection,
) -> Result<ValidatedTranslationPlan, GatewayError> {
    let source_operation = request.source_operation();
    let target_dialect = selection.profile.target_dialect;
    let adapter = provider::for_dialect(target_dialect);
    let target_operation = if source_operation.dialect() == target_dialect {
        source_operation
    } else {
        adapter
            .target_operation(source_operation.canonical())
            .ok_or(GatewayError::UnsupportedRequest)?
    };
    if !selection.supports_target_operation(target_operation) {
        return Err(GatewayError::ModelForbidden);
    }
    let translated = source_operation != target_operation;
    if translated {
        validate_cross_request(request, source_operation, target_operation)?;
    }
    Ok(ValidatedTranslationPlan {
        source_operation,
        target_operation,
        target_dialect,
        translated,
    })
}

fn validate_cross_request(
    request: &CanonicalRequest,
    source: GatewayOperation,
    target: GatewayOperation,
) -> Result<(), GatewayError> {
    let mut seen_tool_calls = std::collections::HashSet::new();
    let mut open_tool_calls = std::collections::HashSet::new();
    if !matches!(
        (source, target),
        (
            GatewayOperation::AnthropicMessages,
            GatewayOperation::OpenAiResponses
        ) | (
            GatewayOperation::OpenAiResponses,
            GatewayOperation::AnthropicMessages
        )
    ) || request.reasoning.is_some()
        || !options_are_empty(&request.provider_options)
        || request.metadata.contains_key("projection_conflicts")
    {
        return Err(GatewayError::UnsupportedRequest);
    }

    // Responses reports input usage only at the terminal event. Anthropic's
    // message_start requires that usage before any content can be emitted, so
    // a streaming translation in this direction cannot be truthful.
    if source == GatewayOperation::AnthropicMessages
        && target == GatewayOperation::OpenAiResponses
        && request.stream
    {
        return Err(GatewayError::UnsupportedRequest);
    }

    for message in &request.messages {
        if message.name.is_some()
            || !options_are_empty(&message.provider_options)
            || matches!(
                message.role,
                CanonicalRole::Developer | CanonicalRole::Other(_)
            )
        {
            return Err(GatewayError::UnsupportedRequest);
        }
        for content in &message.content {
            match content {
                CanonicalContent::Text {
                    provider_options, ..
                } => {
                    if matches!(message.role, CanonicalRole::Tool) {
                        return Err(GatewayError::UnsupportedRequest);
                    }
                    ensure_empty(provider_options)?;
                }
                CanonicalContent::ToolCall(call) => {
                    if !matches!(message.role, CanonicalRole::Assistant) {
                        return Err(GatewayError::UnsupportedRequest);
                    }
                    ensure_empty(&call.provider_options)?;
                    let arguments = match &call.arguments {
                        CanonicalToolArguments::Complete(Value::String(value)) => {
                            serde_json::from_str::<Value>(value)
                                .map_err(|_| GatewayError::UnsupportedRequest)?
                        }
                        CanonicalToolArguments::Complete(value) => value.clone(),
                        CanonicalToolArguments::Delta(_) => {
                            return Err(GatewayError::UnsupportedRequest);
                        }
                    };
                    if !arguments.is_object() || call.id.is_empty() || call.name.is_empty() {
                        return Err(GatewayError::UnsupportedRequest);
                    }
                    if !seen_tool_calls.insert(call.id.as_str())
                        || !open_tool_calls.insert(call.id.as_str())
                    {
                        return Err(GatewayError::UnsupportedRequest);
                    }
                }
                CanonicalContent::ToolResult {
                    tool_call_id,
                    content,
                    provider_options,
                    ..
                } => {
                    if !matches!(message.role, CanonicalRole::User | CanonicalRole::Tool)
                        || !content.is_string()
                    {
                        return Err(GatewayError::UnsupportedRequest);
                    }
                    ensure_empty(provider_options)?;
                    if tool_call_id.is_empty() {
                        return Err(GatewayError::UnsupportedRequest);
                    }
                    if !open_tool_calls.remove(tool_call_id.as_str()) {
                        return Err(GatewayError::UnsupportedRequest);
                    }
                }
                CanonicalContent::Reasoning { .. }
                | CanonicalContent::Image { .. }
                | CanonicalContent::Opaque { .. } => {
                    return Err(GatewayError::UnsupportedRequest);
                }
            }
        }
    }
    if !open_tool_calls.is_empty() {
        return Err(GatewayError::UnsupportedRequest);
    }
    for tool in &request.tools {
        ensure_empty(&tool.provider_options)?;
        if tool.name.is_empty()
            || !supported_json_schema(&tool.input_schema)
            || (tool.strict == Some(true) && !strict_json_schema(&tool.input_schema))
        {
            return Err(GatewayError::UnsupportedRequest);
        }
    }
    if let Some(output) = &request.structured_output
        && (output.kind != CanonicalStructuredOutputKind::JsonSchema
            || output.strict == Some(false)
            || !supported_json_schema(&output.schema)
            || !strict_json_schema(&output.schema)
            || (target == GatewayOperation::AnthropicMessages
                && (output.name.is_some() || output.description.is_some())))
    {
        return Err(GatewayError::UnsupportedRequest);
    }
    if target == GatewayOperation::AnthropicMessages && request.max_output_tokens.is_none() {
        return Err(GatewayError::UnsupportedRequest);
    }
    if source == GatewayOperation::OpenAiResponses
        && target == GatewayOperation::AnthropicMessages
        && request
            .source_document()
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    matches!(
                        item.get("role").and_then(Value::as_str),
                        Some("system" | "developer")
                    )
                })
            })
    {
        return Err(GatewayError::UnsupportedRequest);
    }
    Ok(())
}

fn ensure_empty(options: &ProviderOptions) -> Result<(), GatewayError> {
    options_are_empty(options)
        .then_some(())
        .ok_or(GatewayError::UnsupportedRequest)
}

fn options_are_empty(options: &ProviderOptions) -> bool {
    options
        .values()
        .all(|value| value.as_object().is_some_and(serde_json::Map::is_empty))
}

fn supported_json_schema(schema: &Value) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    if object.keys().any(|key| {
        key != additional_properties_key()
            && !matches!(
                key.as_str(),
                "type" | "properties" | "required" | "items" | "enum" | "description" | "title"
            )
    }) {
        return false;
    }
    if let Some(kind) = object.get("type")
        && !kind.as_str().is_some_and(|value| {
            matches!(
                value,
                "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"
            )
        })
    {
        return false;
    }
    if let Some(properties) = object.get("properties") {
        let Some(properties) = properties.as_object() else {
            return false;
        };
        if !properties.values().all(supported_json_schema) {
            return false;
        }
    }
    if let Some(items) = object.get("items")
        && !supported_json_schema(items)
    {
        return false;
    }
    object.get("required").is_none_or(|value| {
        value
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
    }) && object
        .get(additional_properties_key())
        .is_none_or(Value::is_boolean)
        && object.get("enum").is_none_or(Value::is_array)
}

fn strict_json_schema(schema: &Value) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    if object.get("type").and_then(Value::as_str) == Some("object") {
        let Some(properties) = object.get("properties").and_then(Value::as_object) else {
            return false;
        };
        let required = object
            .get("required")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<std::collections::HashSet<_>>()
            });
        if object
            .get(additional_properties_key())
            .and_then(Value::as_bool)
            != Some(false)
            || required.as_ref().is_none_or(|required| {
                required.len() != properties.len()
                    || properties
                        .keys()
                        .any(|key| !required.contains(key.as_str()))
            })
            || !properties.values().all(strict_json_schema)
        {
            return false;
        }
    }
    object.get("items").is_none_or(strict_json_schema)
}

fn additional_properties_key() -> &'static str {
    concat!("additional", "Properties")
}

pub fn prepare_for_provider(
    request: &CanonicalRequest,
    profile: &ProviderProfile,
    provider_model_id: &str,
    translation: ValidatedTranslationPlan,
    input: ProviderRequestInput<'_>,
) -> Result<provider::PreparedProviderRequest, GatewayError> {
    let native = request.native();
    let adapter = provider::for_dialect(profile.target_dialect);
    if translation.source_operation != native.operation
        || translation.target_dialect != profile.target_dialect
    {
        return Err(GatewayError::InvalidConfiguration);
    }
    let plan = adapter.plan_for_target(translation.target_operation, native.operation, &input)?;
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
    use axum::{
        body::Bytes,
        http::{HeaderMap, HeaderValue},
    };
    use serde_json::Value;
    use uuid::Uuid;

    use super::{ProviderRequestInput, ValidatedTranslationPlan, prepare_for_provider, protocol};
    use crate::{
        canonical::{PrincipalRef, RequestEnrichment},
        model_registry::{GatewayOperation, ProviderDialect, ProviderProfile},
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
                native_plan(*operation),
                input(source_path(*operation), &headers),
            )
            .unwrap();
            assert_eq!(unchanged.body().as_ref(), *body, "unchanged {operation:?}");

            let changed = prepare_for_provider(
                &canonical,
                &profile,
                "provider-model",
                native_plan(*operation),
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
            .plan_for_target(
                GatewayOperation::OpenAiResponses,
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

    #[test]
    fn messages_text_tools_and_schema_encode_as_responses_without_vendor_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static("client-controlled"),
        );
        let canonical = protocol::decode(
            GatewayOperation::AnthropicMessages,
            Bytes::from_static(br#"{
                "model":"public",
                "max_tokens":42,
                "system":"rules",
                "messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}],
                "tools":[{"name":"lookup","description":"find","input_schema":{"type":"object","properties":{"q":{"type":"string"}},"required":["q"],"additionalProperties":false}}],
                "tool_choice":{"type":"auto"},
                "output_config":{"format":{"type":"json_schema","schema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false}}}
            }"#),
        )
        .unwrap()
        .enrich(enrichment());
        super::validate_cross_request(
            &canonical,
            GatewayOperation::AnthropicMessages,
            GatewayOperation::OpenAiResponses,
        )
        .unwrap();
        let prepared = prepare_for_provider(
            &canonical,
            &profile(ProviderDialect::OpenAi),
            "provider-model",
            cross_plan(
                GatewayOperation::AnthropicMessages,
                GatewayOperation::OpenAiResponses,
            ),
            input("/v1/messages", &headers),
        )
        .unwrap();
        let body: Value = serde_json::from_slice(prepared.body()).unwrap();
        assert_eq!(body["model"], "provider-model");
        assert_eq!(body["max_output_tokens"], 42);
        assert_eq!(body["input"][0]["role"], "system");
        assert_eq!(body["input"][1]["content"][0]["text"], "hello");
        assert_eq!(body["tools"][0]["name"], "lookup");
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert!(prepared.headers().get("anthropic-version").is_none());
        assert_eq!(prepared.path_and_query(), "/v1/responses");
    }

    #[test]
    fn responses_text_tools_and_schema_encode_as_messages_with_trusted_header() {
        let mut headers = HeaderMap::new();
        headers.insert("openai-beta", HeaderValue::from_static("client-beta"));
        let canonical = protocol::decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from_static(br#"{
                "model":"public",
                "max_output_tokens":42,
                "instructions":"rules",
                "input":[
                    {"role":"user","content":[{"type":"input_text","text":"hello"}]},
                    {"type":"function_call","call_id":"call_1","name":"lookup","arguments":{"q":"x"}},
                    {"type":"function_call_output","call_id":"call_1","output":"found"}
                ],
                "tools":[{"type":"function","name":"lookup","parameters":{"type":"object","properties":{"q":{"type":"string"}},"required":["q"],"additionalProperties":false},"strict":true}],
                "text":{"format":{"type":"json_schema","schema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false},"strict":true}}
            }"#),
        )
        .unwrap()
        .enrich(enrichment());
        super::validate_cross_request(
            &canonical,
            GatewayOperation::OpenAiResponses,
            GatewayOperation::AnthropicMessages,
        )
        .unwrap();
        let prepared = prepare_for_provider(
            &canonical,
            &profile(ProviderDialect::Anthropic),
            "provider-model",
            cross_plan(
                GatewayOperation::OpenAiResponses,
                GatewayOperation::AnthropicMessages,
            ),
            input("/v1/responses", &headers),
        )
        .unwrap();
        let body: Value = serde_json::from_slice(prepared.body()).unwrap();
        assert_eq!(body["max_tokens"], 42);
        assert_eq!(body["system"][0]["text"], "rules");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hello");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(body["tools"][0]["name"], "lookup");
        assert!(prepared.headers().get("openai-beta").is_none());
        assert_eq!(
            prepared.headers()["anthropic-version"],
            HeaderValue::from_static("2023-06-01")
        );
        assert_eq!(prepared.path_and_query(), "/v1/messages");
    }

    #[test]
    fn cross_translation_rejects_reasoning_vendor_residual_chat_and_count_tokens() {
        let reasoning = protocol::decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from_static(br#"{"model":"public","max_output_tokens":1,"input":"x","reasoning":{"effort":"high"}}"#),
        )
        .unwrap()
        .enrich(enrichment());
        assert!(
            super::validate_cross_request(
                &reasoning,
                GatewayOperation::OpenAiResponses,
                GatewayOperation::AnthropicMessages,
            )
            .is_err()
        );

        let vendor = protocol::decode(
            GatewayOperation::AnthropicMessages,
            Bytes::from_static(
                br#"{"model":"public","max_tokens":1,"messages":[],"temperature":0.2}"#,
            ),
        )
        .unwrap()
        .enrich(enrichment());
        assert!(
            super::validate_cross_request(
                &vendor,
                GatewayOperation::AnthropicMessages,
                GatewayOperation::OpenAiResponses,
            )
            .is_err()
        );

        for (source, target) in [
            (
                GatewayOperation::OpenAiChatCompletions,
                GatewayOperation::AnthropicMessages,
            ),
            (
                GatewayOperation::AnthropicCountTokens,
                GatewayOperation::OpenAiResponses,
            ),
        ] {
            assert!(super::validate_cross_request(&vendor, source, target).is_err());
        }
    }

    #[test]
    fn typed_responses_message_is_translatable_but_reused_tool_id_is_not() {
        let typed = protocol::decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from_static(
                br#"{"model":"public","max_output_tokens":8,"input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}]}"#,
            ),
        )
        .unwrap()
        .enrich(enrichment());
        super::validate_cross_request(
            &typed,
            GatewayOperation::OpenAiResponses,
            GatewayOperation::AnthropicMessages,
        )
        .unwrap();

        let reused = protocol::decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from_static(
                br#"{"model":"public","max_output_tokens":8,"input":[{"type":"function_call","call_id":"same","name":"one","arguments":{}},{"type":"function_call_output","call_id":"same","output":"ok"},{"type":"function_call","call_id":"same","name":"two","arguments":{}},{"type":"function_call_output","call_id":"same","output":"again"}]}"#,
            ),
        )
        .unwrap()
        .enrich(enrichment());
        assert!(
            super::validate_cross_request(
                &reused,
                GatewayOperation::OpenAiResponses,
                GatewayOperation::AnthropicMessages,
            )
            .is_err()
        );

        let reordered_system = protocol::decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from_static(
                br#"{"model":"public","max_output_tokens":8,"input":[{"role":"user","content":"before"},{"role":"system","content":"late policy"}]}"#,
            ),
        )
        .unwrap()
        .enrich(enrichment());
        assert!(
            super::validate_cross_request(
                &reordered_system,
                GatewayOperation::OpenAiResponses,
                GatewayOperation::AnthropicMessages,
            )
            .is_err()
        );

        let streaming = protocol::decode(
            GatewayOperation::AnthropicMessages,
            Bytes::from_static(
                br#"{"model":"public","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
            ),
        )
        .unwrap()
        .enrich(enrichment());
        assert!(
            super::validate_cross_request(
                &streaming,
                GatewayOperation::AnthropicMessages,
                GatewayOperation::OpenAiResponses,
            )
            .is_err()
        );

        let named_schema = protocol::decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from_static(
                br#"{"model":"public","max_output_tokens":8,"input":"hello","text":{"format":{"type":"json_schema","name":"answer","schema":{"type":"object","properties":{},"required":[],"additionalProperties":false},"strict":true}}}"#,
            ),
        )
        .unwrap()
        .enrich(enrichment());
        assert!(
            super::validate_cross_request(
                &named_schema,
                GatewayOperation::OpenAiResponses,
                GatewayOperation::AnthropicMessages,
            )
            .is_err()
        );
    }

    #[test]
    fn generated_responses_tool_output_round_trips_into_anthropic() {
        let request = protocol::decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from_static(
                br#"{"model":"public","max_output_tokens":8,"input":[{"type":"function_call","id":"fc_apg_public_0","call_id":"call_1","name":"lookup","arguments":"{\"q\":\"x\"}","status":"completed"},{"type":"function_call_output","call_id":"call_1","output":"found"}]}"#,
            ),
        )
        .unwrap()
        .enrich(enrichment());
        super::validate_cross_request(
            &request,
            GatewayOperation::OpenAiResponses,
            GatewayOperation::AnthropicMessages,
        )
        .unwrap();
        let prepared = prepare_for_provider(
            &request,
            &profile(ProviderDialect::Anthropic),
            "provider-model",
            cross_plan(
                GatewayOperation::OpenAiResponses,
                GatewayOperation::AnthropicMessages,
            ),
            input("/v1/responses", &HeaderMap::new()),
        )
        .unwrap();
        let body: Value = serde_json::from_slice(prepared.body()).unwrap();
        assert_eq!(body["messages"][0]["content"][0]["id"], "call_1");
        assert_eq!(body["messages"][0]["content"][0]["input"]["q"], "x");
        assert_eq!(body["messages"][1]["content"][0]["tool_use_id"], "call_1");
    }

    fn native_plan(operation: GatewayOperation) -> ValidatedTranslationPlan {
        ValidatedTranslationPlan {
            source_operation: operation,
            target_operation: operation,
            target_dialect: operation.dialect(),
            translated: false,
        }
    }

    fn cross_plan(
        source_operation: GatewayOperation,
        target_operation: GatewayOperation,
    ) -> ValidatedTranslationPlan {
        ValidatedTranslationPlan {
            source_operation,
            target_operation,
            target_dialect: target_operation.dialect(),
            translated: true,
        }
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
