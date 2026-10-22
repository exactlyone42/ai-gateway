use std::collections::HashSet;

use axum::body::Bytes;
use serde_json::{Map, Value, json};

use crate::{
    canonical::{
        CanonicalContent, CanonicalMessage, CanonicalRequest, CanonicalResponse, CanonicalRole,
        CanonicalStructuredOutputKind, CanonicalToolArguments, CanonicalToolCall,
        CanonicalToolChoice, CanonicalUsage, ProviderOptions,
    },
    error::GatewayError,
    model_registry::GatewayOperation,
};

#[cfg(test)]
pub fn transcode_buffered(
    target_operation: GatewayOperation,
    source_operation: GatewayOperation,
    body: &[u8],
    gateway_response_id: &str,
    public_model: &str,
    source_request: &CanonicalRequest,
) -> Result<Bytes, GatewayError> {
    transcode_buffered_with_outcome(
        target_operation,
        source_operation,
        body,
        gateway_response_id,
        public_model,
        source_request,
    )
    .map(|response| response.body)
}

/// Concatenated assistant text of a buffered provider response; any
/// non-text content (tool calls, opaque parts) is ignored.
pub fn buffered_text(
    target_operation: GatewayOperation,
    body: &[u8],
) -> Result<String, GatewayError> {
    let response = match target_operation {
        GatewayOperation::AnthropicMessages => decode_anthropic(body)?,
        GatewayOperation::OpenAiResponses => decode_openai(body)?,
        GatewayOperation::GeminiGenerateContent => decode_gemini(body)?,
        _ => return Err(GatewayError::UnsupportedRequest),
    };
    Ok(response
        .messages
        .iter()
        .filter(|message| message.role == CanonicalRole::Assistant)
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            CanonicalContent::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect())
}

pub struct TranscodedBufferedResponse {
    pub body: Bytes,
    pub incomplete: bool,
}

pub fn transcode_buffered_with_outcome(
    target_operation: GatewayOperation,
    source_operation: GatewayOperation,
    body: &[u8],
    gateway_response_id: &str,
    public_model: &str,
    source_request: &CanonicalRequest,
) -> Result<TranscodedBufferedResponse, GatewayError> {
    let response = match target_operation {
        GatewayOperation::AnthropicMessages => decode_anthropic(body)?,
        GatewayOperation::OpenAiResponses => decode_openai(body)?,
        GatewayOperation::GeminiGenerateContent => decode_gemini(body)?,
        _ => return Err(GatewayError::UnsupportedRequest),
    };
    let incomplete = matches!(response.finish_reason.as_deref(), Some("max_tokens"));
    let value = match source_operation {
        GatewayOperation::AnthropicMessages => {
            encode_anthropic(response, gateway_response_id, public_model)?
        }
        GatewayOperation::OpenAiResponses => {
            encode_openai(response, gateway_response_id, public_model, source_request)?
        }
        _ => return Err(GatewayError::UnsupportedRequest),
    };
    let body = serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(GatewayError::InvalidRequest)?;
    Ok(TranscodedBufferedResponse { body, incomplete })
}

fn decode_gemini(body: &[u8]) -> Result<CanonicalResponse, GatewayError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| GatewayError::UpstreamProtocol)?;
    let response = super::gemini_wire::decode(value)?;
    let candidate = response
        .candidates
        .first()
        .ok_or(GatewayError::UpstreamProtocol)?;
    let content = candidate
        .content
        .parts
        .iter()
        .map(|part| {
            part.text
                .as_ref()
                .filter(|_| part.function_call.is_none())
                .map(|text| CanonicalContent::Text {
                    text: text.clone(),
                    provider_options: ProviderOptions::new(),
                })
                .ok_or(GatewayError::UpstreamProtocol)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let finish_reason = match candidate.finish_reason.as_deref() {
        Some("STOP") => Some("stop".to_owned()),
        Some("MAX_TOKENS") => Some("max_tokens".to_owned()),
        _ => return Err(GatewayError::UpstreamProtocol),
    };
    let usage = super::provider::usage_for(crate::model_registry::ProviderKind::Gemini, body)?
        .ok_or(GatewayError::UpstreamProtocol)?;
    Ok(CanonicalResponse {
        response_id: None,
        model: None,
        messages: vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            name: None,
            content,
            provider_options: ProviderOptions::new(),
        }],
        usage,
        finish_reason,
        error: None,
        provider_options: ProviderOptions::new(),
        metadata: Default::default(),
    })
}

fn decode_anthropic(body: &[u8]) -> Result<CanonicalResponse, GatewayError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| GatewayError::UpstreamProtocol)?;
    if value.get("type").and_then(Value::as_str) != Some("message")
        || value.get("role").and_then(Value::as_str) != Some("assistant")
    {
        return Err(GatewayError::UpstreamProtocol);
    }
    let finish_reason = optional_string(&value, "stop_reason")?;
    validate_anthropic_finish_reason(finish_reason.as_deref())?;
    let mut tool_ids = HashSet::new();
    let content = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or(GatewayError::UpstreamProtocol)?
        .iter()
        .map(|content| decode_anthropic_content(content, &mut tool_ids))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CanonicalResponse {
        response_id: required_string(&value, "id").map(Some)?,
        model: optional_string(&value, "model")?,
        messages: vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            name: None,
            content,
            provider_options: ProviderOptions::new(),
        }],
        usage: anthropic_usage(value.get("usage"))?,
        finish_reason,
        error: None,
        provider_options: ProviderOptions::new(),
        metadata: Default::default(),
    })
}

fn decode_anthropic_content(
    value: &Value,
    tool_ids: &mut HashSet<String>,
) -> Result<CanonicalContent, GatewayError> {
    match value.get("type").and_then(Value::as_str) {
        Some("text") => {
            require_only_fields(value, &["type", "text"])?;
            Ok(CanonicalContent::Text {
                text: required_string(value, "text")?,
                provider_options: ProviderOptions::new(),
            })
        }
        Some("tool_use") => {
            require_only_fields(value, &["type", "id", "name", "input"])?;
            let id = required_string(value, "id")?;
            if !tool_ids.insert(id.clone()) {
                return Err(GatewayError::UpstreamProtocol);
            }
            Ok(CanonicalContent::ToolCall(CanonicalToolCall {
                id,
                name: required_string(value, "name")?,
                arguments: CanonicalToolArguments::Complete(
                    value
                        .get("input")
                        .filter(|input| input.is_object())
                        .cloned()
                        .ok_or(GatewayError::UpstreamProtocol)?,
                ),
                provider_options: ProviderOptions::new(),
            }))
        }
        _ => Err(GatewayError::UpstreamProtocol),
    }
}

fn decode_openai(body: &[u8]) -> Result<CanonicalResponse, GatewayError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| GatewayError::UpstreamProtocol)?;
    if value.get("object").and_then(Value::as_str) != Some("response") {
        return Err(GatewayError::UpstreamProtocol);
    }
    let response_status = required_string(&value, "status")?;
    let incomplete = match response_status.as_str() {
        "completed" => false,
        "incomplete"
            if value
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
                == Some("max_output_tokens") =>
        {
            true
        }
        _ => return Err(GatewayError::UpstreamProtocol),
    };
    let mut content = Vec::new();
    let mut used_tool = false;
    let mut item_ids = HashSet::new();
    let mut call_ids = HashSet::new();
    for item in value
        .get("output")
        .and_then(Value::as_array)
        .ok_or(GatewayError::UpstreamProtocol)?
    {
        let item_id = required_string(item, "id")?;
        let item_status = item.get("status").and_then(Value::as_str);
        let valid_item_status =
            item_status == Some("completed") || (incomplete && item_status == Some("incomplete"));
        if !item_ids.insert(item_id) || !valid_item_status {
            return Err(GatewayError::UpstreamProtocol);
        }
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if item.get("role").and_then(Value::as_str) != Some("assistant") {
                    return Err(GatewayError::UpstreamProtocol);
                }
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or(GatewayError::UpstreamProtocol)?
                {
                    if part.get("type").and_then(Value::as_str) != Some("output_text") {
                        return Err(GatewayError::UpstreamProtocol);
                    }
                    require_only_fields(part, &["type", "text", "annotations", "logprobs"])?;
                    require_empty_array_if_present(part, "annotations")?;
                    require_empty_array_if_present(part, "logprobs")?;
                    content.push(CanonicalContent::Text {
                        text: required_string(part, "text")?,
                        provider_options: ProviderOptions::new(),
                    });
                }
            }
            Some("function_call") => {
                require_only_fields(
                    item,
                    &["type", "id", "status", "call_id", "name", "arguments"],
                )?;
                used_tool = true;
                let call_id = required_string(item, "call_id")?;
                if !call_ids.insert(call_id.clone()) {
                    return Err(GatewayError::UpstreamProtocol);
                }
                let arguments = required_string(item, "arguments")?;
                let arguments = serde_json::from_str::<Value>(&arguments)
                    .ok()
                    .filter(Value::is_object)
                    .ok_or(GatewayError::UpstreamProtocol)?;
                content.push(CanonicalContent::ToolCall(CanonicalToolCall {
                    id: call_id,
                    name: required_string(item, "name")?,
                    arguments: CanonicalToolArguments::Complete(arguments),
                    provider_options: ProviderOptions::new(),
                }));
            }
            _ => return Err(GatewayError::UpstreamProtocol),
        }
    }
    Ok(CanonicalResponse {
        response_id: required_string(&value, "id").map(Some)?,
        model: optional_string(&value, "model")?,
        messages: vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            name: None,
            content,
            provider_options: ProviderOptions::new(),
        }],
        usage: openai_usage(value.get("usage"))?,
        finish_reason: Some(
            if incomplete {
                "max_tokens"
            } else if used_tool {
                "tool_use"
            } else {
                "stop"
            }
            .to_owned(),
        ),
        error: None,
        provider_options: ProviderOptions::new(),
        metadata: Default::default(),
    })
}

fn encode_anthropic(
    response: CanonicalResponse,
    response_id: &str,
    public_model: &str,
) -> Result<Value, GatewayError> {
    let mut content = Vec::new();
    for message in response.messages {
        for item in message.content {
            match item {
                CanonicalContent::Text { text, .. } => {
                    content.push(json!({"type": "text", "text": text}));
                }
                CanonicalContent::ToolCall(call) => content.push(json!({
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": complete_arguments(call.arguments)?
                })),
                _ => return Err(GatewayError::UpstreamProtocol),
            }
        }
    }
    Ok(json!({
        "id": response_id,
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": public_model,
        "stop_reason": anthropic_finish_reason(response.finish_reason.as_deref())?,
        "stop_sequence": null,
        "usage": {
            "input_tokens": response.usage.input_tokens,
            "output_tokens": response.usage.output_tokens,
            "cache_read_input_tokens": response.usage.cached_input_tokens,
            "cache_creation_input_tokens": response.usage.cache_write_input_tokens
        }
    }))
}

fn encode_openai(
    response: CanonicalResponse,
    response_id: &str,
    public_model: &str,
    source_request: &CanonicalRequest,
) -> Result<Value, GatewayError> {
    let mut output = Vec::new();
    let mut message_content = Vec::new();
    let mut ordinal = 0usize;
    for message in response.messages {
        for item in message.content {
            match item {
                CanonicalContent::Text { text, .. } => message_content.push(json!({
                    "type": "output_text",
                    "text": text,
                    "annotations": []
                })),
                CanonicalContent::ToolCall(call) => {
                    flush_openai_text(&mut output, &mut message_content, response_id, &mut ordinal);
                    output.push(json!({
                        "type": "function_call",
                        "id": format!("fc_{}_{}", response_id, ordinal),
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": complete_arguments(call.arguments)?.to_string(),
                        "status": "completed"
                    }));
                    ordinal += 1;
                }
                _ => return Err(GatewayError::UpstreamProtocol),
            }
        }
    }
    flush_openai_text(&mut output, &mut message_content, response_id, &mut ordinal);
    let incomplete = matches!(response.finish_reason.as_deref(), Some("max_tokens"));
    let status = if incomplete {
        "incomplete"
    } else {
        "completed"
    };
    let incomplete_details = incomplete.then(|| json!({"reason": "max_output_tokens"}));
    let input_tokens = response
        .usage
        .input_tokens
        .saturating_add(response.usage.cached_input_tokens)
        .saturating_add(response.usage.cache_write_input_tokens);
    let output_tokens = response
        .usage
        .output_tokens
        .saturating_add(response.usage.reasoning_tokens);
    let usage = json!({
        "input_tokens": input_tokens,
        "input_tokens_details": {"cached_tokens": response.usage.cached_input_tokens},
        "output_tokens": output_tokens,
        "output_tokens_details": {"reasoning_tokens": response.usage.reasoning_tokens},
        "total_tokens": input_tokens.saturating_add(output_tokens)
    });
    let mut envelope = openai_response_envelope(
        source_request,
        response_id,
        public_model,
        chrono::Utc::now().timestamp(),
        status,
        Value::Array(output),
        Some(usage),
    );
    envelope["incomplete_details"] = incomplete_details.unwrap_or(Value::Null);
    Ok(envelope)
}

pub(crate) fn openai_response_envelope(
    request: &CanonicalRequest,
    response_id: &str,
    public_model: &str,
    created_at: i64,
    status: &str,
    output: Value,
    usage: Option<Value>,
) -> Value {
    let tools = request
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
        .collect::<Vec<_>>();
    let tool_choice = request.tool_choice.as_ref().map_or_else(
        || json!("auto"),
        |choice| match choice {
            CanonicalToolChoice::Auto => json!("auto"),
            CanonicalToolChoice::Required => json!("required"),
            CanonicalToolChoice::None => json!("none"),
            CanonicalToolChoice::Named(name) => json!({"type": "function", "name": name}),
        },
    );
    let text_format = request.structured_output.as_ref().map_or_else(
        || json!({"type": "text"}),
        |format| match format.kind {
            CanonicalStructuredOutputKind::JsonSchema => json!({
                "type": "json_schema",
                "name": format.name.as_deref().unwrap_or("response"),
                "description": format.description,
                "schema": format.schema,
                "strict": true
            }),
            CanonicalStructuredOutputKind::JsonObject => json!({"type": "json_object"}),
        },
    );
    json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": status,
        "error": null,
        "incomplete_details": null,
        "instructions": request
            .source_document()
            .get("instructions")
            .cloned()
            .unwrap_or(Value::Null),
        "max_output_tokens": request.max_output_tokens,
        "model": public_model,
        "output": output,
        "parallel_tool_calls": true,
        "previous_response_id": null,
        "reasoning": {"effort": null, "summary": null},
        "store": false,
        "temperature": null,
        "text": {"format": text_format},
        "tool_choice": tool_choice,
        "tools": tools,
        "top_p": null,
        "truncation": "disabled",
        "usage": usage,
        "user": null,
        "metadata": {}
    })
}

fn flush_openai_text(
    output: &mut Vec<Value>,
    content: &mut Vec<Value>,
    response_id: &str,
    ordinal: &mut usize,
) {
    if content.is_empty() {
        return;
    }
    output.push(json!({
        "type": "message",
        "id": format!("msg_{}_{}", response_id, ordinal),
        "status": "completed",
        "role": "assistant",
        "content": std::mem::take(content)
    }));
    *ordinal += 1;
}

fn anthropic_usage(value: Option<&Value>) -> Result<CanonicalUsage, GatewayError> {
    let value = value
        .and_then(Value::as_object)
        .ok_or(GatewayError::UpstreamProtocol)?;
    Ok(CanonicalUsage {
        input_tokens: number(value, "input_tokens")?,
        output_tokens: number(value, "output_tokens")?,
        cached_input_tokens: optional_number(value, "cache_read_input_tokens")?,
        cache_write_input_tokens: optional_number(value, "cache_creation_input_tokens")?,
        reasoning_tokens: 0,
        provider_options: ProviderOptions::new(),
    })
}

fn openai_usage(value: Option<&Value>) -> Result<CanonicalUsage, GatewayError> {
    let value = value
        .and_then(Value::as_object)
        .ok_or(GatewayError::UpstreamProtocol)?;
    Ok(CanonicalUsage {
        input_tokens: number(value, "input_tokens")?,
        output_tokens: number(value, "output_tokens")?,
        cached_input_tokens: nested_number(value, "input_tokens_details", "cached_tokens")?,
        cache_write_input_tokens: nested_number(
            value,
            "input_tokens_details",
            "cache_write_tokens",
        )?,
        reasoning_tokens: nested_number(value, "output_tokens_details", "reasoning_tokens")?,
        provider_options: ProviderOptions::new(),
    })
}

fn required_string(value: &Value, key: &str) -> Result<String, GatewayError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or(GatewayError::UpstreamProtocol)
}

fn optional_string(value: &Value, key: &str) -> Result<Option<String>, GatewayError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(GatewayError::UpstreamProtocol),
    }
}

fn require_only_fields(value: &Value, allowed: &[&str]) -> Result<(), GatewayError> {
    value
        .as_object()
        .filter(|object| object.keys().all(|key| allowed.contains(&key.as_str())))
        .map(|_| ())
        .ok_or(GatewayError::UpstreamProtocol)
}

fn require_empty_array_if_present(value: &Value, key: &str) -> Result<(), GatewayError> {
    match value.get(key) {
        None => Ok(()),
        Some(Value::Array(values)) if values.is_empty() => Ok(()),
        Some(_) => Err(GatewayError::UpstreamProtocol),
    }
}

fn complete_arguments(arguments: CanonicalToolArguments) -> Result<Value, GatewayError> {
    match arguments {
        CanonicalToolArguments::Complete(value) if value.is_object() => Ok(value),
        _ => Err(GatewayError::UpstreamProtocol),
    }
}

fn number(value: &Map<String, Value>, key: &str) -> Result<u64, GatewayError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(GatewayError::UpstreamProtocol)
}

fn optional_number(value: &Map<String, Value>, key: &str) -> Result<u64, GatewayError> {
    match value.get(key) {
        None => Ok(0),
        Some(value) => value.as_u64().ok_or(GatewayError::UpstreamProtocol),
    }
}

fn nested_number(value: &Map<String, Value>, parent: &str, key: &str) -> Result<u64, GatewayError> {
    value
        .get(parent)
        .and_then(Value::as_object)
        .map_or(Ok(0), |nested| optional_number(nested, key))
}

fn anthropic_finish_reason(reason: Option<&str>) -> Result<&'static str, GatewayError> {
    match reason {
        Some("max_tokens" | "max_output_tokens" | "length") => Ok("max_tokens"),
        Some("tool_use") => Ok("tool_use"),
        Some("stop" | "end_turn") => Ok("end_turn"),
        _ => Err(GatewayError::UpstreamProtocol),
    }
}

fn validate_anthropic_finish_reason(reason: Option<&str>) -> Result<(), GatewayError> {
    match reason {
        Some("end_turn" | "max_tokens" | "tool_use") => Ok(()),
        _ => Err(GatewayError::UpstreamProtocol),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Bytes;
    use serde_json::Value;
    use uuid::Uuid;

    use super::{transcode_buffered, transcode_buffered_with_outcome};
    use crate::{
        adapters::protocol,
        canonical::{PrincipalRef, RequestEnrichment},
        model_registry::GatewayOperation,
    };

    fn source_request(operation: GatewayOperation) -> crate::canonical::CanonicalRequest {
        let body = match operation {
            GatewayOperation::AnthropicMessages => {
                br#"{"model":"public","max_tokens":8,"messages":[{"role":"user","content":"hello"}]}"#.as_slice()
            }
            GatewayOperation::OpenAiResponses => {
                br#"{"model":"public","max_output_tokens":8,"input":"hello"}"#.as_slice()
            }
            _ => unreachable!(),
        };
        source_request_from_body(operation, body)
    }

    fn source_request_from_body(
        operation: GatewayOperation,
        body: &[u8],
    ) -> crate::canonical::CanonicalRequest {
        protocol::decode(operation, Bytes::copy_from_slice(body))
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

    #[test]
    fn anthropic_response_becomes_responses_without_exposing_target_id() {
        let body = br#"{
            "id":"msg_target_secret",
            "type":"message",
            "role":"assistant",
            "model":"target-model",
            "content":[
                {"type":"text","text":"hello"},
                {"type":"tool_use","id":"call_1","name":"lookup","input":{"q":"x"}}
            ],
            "stop_reason":"tool_use",
            "usage":{"input_tokens":7,"output_tokens":3,"cache_read_input_tokens":2}
        }"#;
        let encoded = transcode_buffered(
            GatewayOperation::AnthropicMessages,
            GatewayOperation::OpenAiResponses,
            body,
            "resp_apg_public",
            "public-model",
            &source_request(GatewayOperation::OpenAiResponses),
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["id"], "resp_apg_public");
        assert_eq!(value["model"], "public-model");
        assert_eq!(value["output"][0]["content"][0]["text"], "hello");
        assert_eq!(value["output"][1]["call_id"], "call_1");
        assert_eq!(value["usage"]["input_tokens"], 9);
        assert!(
            !String::from_utf8(encoded.to_vec())
                .unwrap()
                .contains("msg_target_secret")
        );
    }

    #[test]
    fn gemini_buffered_response_becomes_responses_with_usage() {
        let body = br#"{
            "candidates":[{"content":{"role":"model","parts":[{"text":"hello"}]},"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":7,"cachedContentTokenCount":2,"candidatesTokenCount":3,"thoughtsTokenCount":1}
        }"#;
        let encoded = transcode_buffered(
            GatewayOperation::GeminiGenerateContent,
            GatewayOperation::OpenAiResponses,
            body,
            "resp_apg_public",
            "public-model",
            &source_request(GatewayOperation::OpenAiResponses),
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["output"][0]["content"][0]["text"], "hello");
        assert_eq!(value["usage"]["input_tokens"], 7);
        assert_eq!(value["usage"]["input_tokens_details"]["cached_tokens"], 2);
        assert_eq!(value["usage"]["output_tokens"], 4);
        assert_eq!(
            value["usage"]["output_tokens_details"]["reasoning_tokens"],
            1
        );
    }

    #[test]
    fn responses_response_becomes_anthropic_with_target_usage() {
        let body = br#"{
            "id":"resp_target_secret",
            "object":"response",
            "status":"completed",
            "model":"target-model",
            "output":[
                {"type":"message","id":"item_1","status":"completed","role":"assistant","content":[{"type":"output_text","text":"hello"}]},
                {"type":"function_call","id":"item_2","status":"completed","call_id":"call_1","name":"lookup","arguments":"{\"q\":\"x\"}"}
            ],
            "usage":{"input_tokens":11,"output_tokens":4,"input_tokens_details":{"cached_tokens":3},"output_tokens_details":{"reasoning_tokens":0}}
        }"#;
        let encoded = transcode_buffered(
            GatewayOperation::OpenAiResponses,
            GatewayOperation::AnthropicMessages,
            body,
            "msg_apg_public",
            "public-model",
            &source_request(GatewayOperation::AnthropicMessages),
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["id"], "msg_apg_public");
        assert_eq!(value["model"], "public-model");
        assert_eq!(value["content"][0]["text"], "hello");
        assert_eq!(value["content"][1]["input"]["q"], "x");
        assert_eq!(value["stop_reason"], "tool_use");
        assert_eq!(value["usage"]["input_tokens"], 11);
        assert!(
            !String::from_utf8(encoded.to_vec())
                .unwrap()
                .contains("resp_target_secret")
        );
    }

    #[test]
    fn parallel_tools_keep_order_and_unique_public_ids() {
        let body = br#"{
            "id":"msg_target_secret","type":"message","role":"assistant","model":"target-model",
            "content":[
                {"type":"tool_use","id":"call_1","name":"first","input":{}},
                {"type":"text","text":"between"},
                {"type":"tool_use","id":"call_2","name":"second","input":{}}
            ],
            "stop_reason":"tool_use",
            "usage":{"input_tokens":1,"output_tokens":1}
        }"#;
        let encoded = transcode_buffered(
            GatewayOperation::AnthropicMessages,
            GatewayOperation::OpenAiResponses,
            body,
            "resp_apg_public",
            "public-model",
            &source_request(GatewayOperation::OpenAiResponses),
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["output"][0]["call_id"], "call_1");
        assert_eq!(value["output"][1]["content"][0]["text"], "between");
        assert_eq!(value["output"][2]["call_id"], "call_2");
        assert_ne!(value["output"][0]["id"], value["output"][2]["id"]);
    }

    #[test]
    fn unknown_target_content_fails_closed() {
        let body = br#"{"id":"msg_target","content":[{"type":"thinking","thinking":"secret"}],"usage":{"input_tokens":1,"output_tokens":1}}"#;
        assert!(
            transcode_buffered(
                GatewayOperation::AnthropicMessages,
                GatewayOperation::OpenAiResponses,
                body,
                "resp_apg_public",
                "public-model",
                &source_request(GatewayOperation::OpenAiResponses),
            )
            .is_err()
        );
    }

    #[test]
    fn buffered_response_validation_rejects_lossy_or_ambiguous_topology() {
        let bad_stop = br#"{
            "id":"msg_target","type":"message","role":"assistant",
            "content":[{"type":"text","text":"later"}],
            "stop_reason":"pause_turn","usage":{"input_tokens":1,"output_tokens":1}
        }"#;
        let duplicate_tool = br#"{
            "id":"msg_target","type":"message","role":"assistant",
            "content":[
                {"type":"tool_use","id":"same","name":"one","input":{}},
                {"type":"tool_use","id":"same","name":"two","input":{}}
            ],
            "stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}
        }"#;
        let wrong_openai_item = br#"{
            "id":"resp_target","object":"response","status":"completed",
            "output":[{"type":"message","id":"item_1","status":"in_progress","role":"user","content":[]}],
            "usage":{"input_tokens":1,"output_tokens":1}
        }"#;
        let duplicate_openai_call = br#"{
            "id":"resp_target","object":"response","status":"completed",
            "output":[
                {"type":"function_call","id":"item_1","status":"completed","call_id":"same","name":"one","arguments":"{}"},
                {"type":"function_call","id":"item_2","status":"completed","call_id":"same","name":"two","arguments":"{}"}
            ],
            "usage":{"input_tokens":1,"output_tokens":1}
        }"#;
        for body in [bad_stop.as_slice(), duplicate_tool.as_slice()] {
            assert!(
                transcode_buffered(
                    GatewayOperation::AnthropicMessages,
                    GatewayOperation::OpenAiResponses,
                    body,
                    "resp_apg_public",
                    "public-model",
                    &source_request(GatewayOperation::OpenAiResponses),
                )
                .is_err()
            );
        }
        assert!(
            transcode_buffered(
                GatewayOperation::OpenAiResponses,
                GatewayOperation::AnthropicMessages,
                wrong_openai_item,
                "msg_apg_public",
                "public-model",
                &source_request(GatewayOperation::AnthropicMessages),
            )
            .is_err()
        );
        assert!(
            transcode_buffered(
                GatewayOperation::OpenAiResponses,
                GatewayOperation::AnthropicMessages,
                duplicate_openai_call,
                "msg_apg_public",
                "public-model",
                &source_request(GatewayOperation::AnthropicMessages),
            )
            .is_err()
        );
    }

    #[test]
    fn responses_usage_and_anthropic_request_echo_survive_buffered_translation() {
        let openai = br#"{
            "id":"resp_target","object":"response","status":"completed",
            "output":[{"type":"message","id":"item_1","status":"completed","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],
            "usage":{"input_tokens":11,"output_tokens":4,"input_tokens_details":{"cached_tokens":3,"cache_write_tokens":5}}
        }"#;
        let anthropic = transcode_buffered(
            GatewayOperation::OpenAiResponses,
            GatewayOperation::AnthropicMessages,
            openai,
            "msg_apg_public",
            "public-model",
            &source_request(GatewayOperation::AnthropicMessages),
        )
        .unwrap();
        let anthropic: Value = serde_json::from_slice(&anthropic).unwrap();
        assert_eq!(anthropic["usage"]["cache_creation_input_tokens"], 5);

        let source = source_request_from_body(
            GatewayOperation::AnthropicMessages,
            br#"{
                "model":"public","max_tokens":8,"messages":[{"role":"user","content":"hello"}],
                "tools":[{"name":"lookup","input_schema":{"type":"object","properties":{},"required":[],"additionalProperties":false}}],
                "tool_choice":{"type":"any"},
                "output_config":{"format":{"type":"json_schema","schema":{"type":"object","properties":{},"required":[],"additionalProperties":false}}}
            }"#,
        );
        let target = br#"{
            "id":"msg_target","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],
            "stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}
        }"#;
        let responses = transcode_buffered(
            GatewayOperation::AnthropicMessages,
            GatewayOperation::OpenAiResponses,
            target,
            "resp_apg_public",
            "public-model",
            &source,
        )
        .unwrap();
        let responses: Value = serde_json::from_slice(&responses).unwrap();
        assert_eq!(responses["max_output_tokens"], 8);
        assert_eq!(responses["tools"][0]["name"], "lookup");
        assert_eq!(responses["tool_choice"], "required");
        assert_eq!(responses["text"]["format"]["type"], "json_schema");
    }

    #[test]
    fn incomplete_responses_keep_partial_output_and_terminal_status() {
        let openai = br#"{
            "id":"resp_target","object":"response","status":"incomplete",
            "incomplete_details":{"reason":"max_output_tokens"},
            "output":[{"type":"message","id":"item_1","status":"incomplete","role":"assistant","content":[{"type":"output_text","text":"partial","annotations":[]}]}],
            "usage":{"input_tokens":11,"output_tokens":4}
        }"#;
        let anthropic = transcode_buffered_with_outcome(
            GatewayOperation::OpenAiResponses,
            GatewayOperation::AnthropicMessages,
            openai,
            "msg_apg_public",
            "public-model",
            &source_request(GatewayOperation::AnthropicMessages),
        )
        .unwrap();
        assert!(anthropic.incomplete);
        let anthropic: Value = serde_json::from_slice(&anthropic.body).unwrap();
        assert_eq!(anthropic["stop_reason"], "max_tokens");
        assert_eq!(anthropic["content"][0]["text"], "partial");

        let invalid_completed = br#"{
            "id":"resp_target","object":"response","status":"completed",
            "output":[{"type":"message","id":"item_1","status":"incomplete","role":"assistant","content":[{"type":"output_text","text":"partial","annotations":[]}]}],
            "usage":{"input_tokens":11,"output_tokens":4}
        }"#;
        assert!(
            transcode_buffered_with_outcome(
                GatewayOperation::OpenAiResponses,
                GatewayOperation::AnthropicMessages,
                invalid_completed,
                "msg_apg_public",
                "public-model",
                &source_request(GatewayOperation::AnthropicMessages),
            )
            .is_err()
        );

        let source = source_request_from_body(
            GatewayOperation::OpenAiResponses,
            br#"{"model":"public","instructions":"be exact","input":"hello","max_output_tokens":8}"#,
        );
        let target = br#"{
            "id":"msg_target","type":"message","role":"assistant",
            "content":[{"type":"text","text":"partial"}],
            "stop_reason":"max_tokens","usage":{"input_tokens":3,"output_tokens":8}
        }"#;
        let responses = transcode_buffered_with_outcome(
            GatewayOperation::AnthropicMessages,
            GatewayOperation::OpenAiResponses,
            target,
            "resp_apg_public",
            "public-model",
            &source,
        )
        .unwrap();
        assert!(responses.incomplete);
        let responses: Value = serde_json::from_slice(&responses.body).unwrap();
        assert_eq!(responses["status"], "incomplete");
        assert_eq!(responses["instructions"], "be exact");
    }

    #[test]
    fn nonempty_citations_and_annotations_fail_closed() {
        let anthropic = br#"{
            "id":"msg_target","type":"message","role":"assistant",
            "content":[{"type":"text","text":"cited","citations":[{"type":"page_location","cited_text":"x"}]}],
            "stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}
        }"#;
        assert!(
            transcode_buffered(
                GatewayOperation::AnthropicMessages,
                GatewayOperation::OpenAiResponses,
                anthropic,
                "resp_apg_public",
                "public-model",
                &source_request(GatewayOperation::OpenAiResponses),
            )
            .is_err()
        );

        let openai = br#"{
            "id":"resp_target","object":"response","status":"completed",
            "output":[{"type":"message","id":"item_1","status":"completed","role":"assistant","content":[{"type":"output_text","text":"cited","annotations":[{"type":"url_citation","url":"https://example.test"}]}]}],
            "usage":{"input_tokens":1,"output_tokens":1}
        }"#;
        assert!(
            transcode_buffered(
                GatewayOperation::OpenAiResponses,
                GatewayOperation::AnthropicMessages,
                openai,
                "msg_apg_public",
                "public-model",
                &source_request(GatewayOperation::AnthropicMessages),
            )
            .is_err()
        );
    }
}
