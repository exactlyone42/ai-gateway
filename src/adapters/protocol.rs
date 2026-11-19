use std::collections::BTreeMap;

use axum::body::Bytes;
use serde_json::{Map, Value};

use crate::{
    canonical::{
        CanonicalContent, CanonicalMessage, CanonicalReasoning, CanonicalRequest,
        CanonicalRequestPayload, CanonicalRole, CanonicalStructuredOutput,
        CanonicalStructuredOutputKind, CanonicalToolArguments, CanonicalToolCall,
        CanonicalToolChoice, CanonicalToolDefinition, NativeWire, ProviderOptions,
        RequestEnrichment,
    },
    model_registry::{GatewayOperation, parse_request},
};

pub struct DecodedProtocolRequest {
    operation: GatewayOperation,
    body: Bytes,
    document: Value,
    requested_model: String,
    requested_quality: Option<String>,
    messages: Vec<CanonicalMessage>,
    tools: Vec<CanonicalToolDefinition>,
    tool_choice: Option<CanonicalToolChoice>,
    reasoning: Option<CanonicalReasoning>,
    structured_output: Option<CanonicalStructuredOutput>,
    stream: bool,
    max_output_tokens: Option<u64>,
    provider_options: ProviderOptions,
    metadata: BTreeMap<String, Value>,
}

pub fn decode(
    operation: GatewayOperation,
    body: Bytes,
) -> Result<DecodedProtocolRequest, serde_json::Error> {
    let document = parse_request(&body)?;
    let requested_model = document
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let requested_quality = requested_model
        .strip_prefix("quality:")
        .map(ToOwned::to_owned);
    let mut residual = document.as_object().cloned().unwrap_or_default();
    residual.remove("model");

    let stream = take_bool(&mut residual, "stream").unwrap_or(false);
    let (max_output_tokens, output_limit_conflict) = take_output_limit(operation, &mut residual);
    let tool_choice = take_tool_choice(operation, &mut residual);
    let reasoning_conflict = matches!(operation, GatewayOperation::OpenAiChatCompletions)
        && residual.contains_key("reasoning_effort")
        && residual.contains_key("reasoning");
    let reasoning = if reasoning_conflict {
        None
    } else {
        take_reasoning(operation, &mut residual)
    };
    let structured_output = take_structured_output(operation, &mut residual);
    let tools = take_tools(operation, &mut residual);
    let messages = take_messages(operation, &mut residual);
    let provider = source_provider(operation);
    let provider_options = scoped_options(provider, residual);
    let mut metadata = BTreeMap::from([(
        "source_operation".to_owned(),
        Value::String(operation.as_str().to_owned()),
    )]);
    let conflicts = [
        output_limit_conflict.then_some("multiple_output_limit_aliases"),
        reasoning_conflict.then_some("multiple_reasoning_aliases"),
    ]
    .into_iter()
    .flatten()
    .map(|value| Value::String(value.to_owned()))
    .collect::<Vec<_>>();
    if !conflicts.is_empty() {
        metadata.insert("projection_conflicts".to_owned(), Value::Array(conflicts));
    }

    Ok(DecodedProtocolRequest {
        operation,
        body,
        document,
        requested_model,
        requested_quality,
        messages,
        tools,
        tool_choice,
        reasoning,
        structured_output,
        stream,
        max_output_tokens,
        provider_options,
        metadata,
    })
}

impl DecodedProtocolRequest {
    pub fn document(&self) -> &Value {
        &self.document
    }

    pub fn requested_model(&self) -> &str {
        &self.requested_model
    }

    pub fn enrich(self, mut enrichment: RequestEnrichment) -> CanonicalRequest {
        enrichment.requested_quality = self.requested_quality;
        CanonicalRequest::from_parts(
            enrichment,
            CanonicalRequestPayload {
                messages: self.messages,
                tools: self.tools,
                tool_choice: self.tool_choice,
                reasoning: self.reasoning,
                structured_output: self.structured_output,
                stream: self.stream,
                max_output_tokens: self.max_output_tokens,
                provider_options: self.provider_options,
                metadata: self.metadata,
            },
            NativeWire {
                operation: self.operation,
                body: self.body,
                document: self.document,
            },
        )
    }
}

fn source_provider(operation: GatewayOperation) -> &'static str {
    match operation {
        GatewayOperation::AnthropicMessages | GatewayOperation::AnthropicCountTokens => "anthropic",
        GatewayOperation::OpenAiResponses | GatewayOperation::OpenAiChatCompletions => "openai",
        GatewayOperation::GeminiGenerateContent => "gemini",
    }
}

fn take_bool(object: &mut Map<String, Value>, key: &str) -> Option<bool> {
    let value = object.get(key)?.as_bool()?;
    object.remove(key);
    Some(value)
}

fn take_output_limit(
    operation: GatewayOperation,
    object: &mut Map<String, Value>,
) -> (Option<u64>, bool) {
    let keys: &[&str] = match operation {
        GatewayOperation::AnthropicMessages => &["max_tokens"],
        GatewayOperation::AnthropicCountTokens => return (Some(0), false),
        GatewayOperation::OpenAiResponses => &["max_output_tokens"],
        GatewayOperation::OpenAiChatCompletions => &["max_completion_tokens", "max_tokens"],
        GatewayOperation::GeminiGenerateContent => return (None, false),
    };
    let present = keys
        .iter()
        .filter(|key| object.contains_key(**key))
        .copied()
        .collect::<Vec<_>>();
    if present.len() > 1 {
        return (None, true);
    }
    let Some(key) = present.first() else {
        return (None, false);
    };
    let Some(value) = object.get(*key).and_then(Value::as_u64) else {
        return (None, false);
    };
    object.remove(*key);
    (Some(value), false)
}

fn take_tool_choice(
    operation: GatewayOperation,
    object: &mut Map<String, Value>,
) -> Option<CanonicalToolChoice> {
    let value = object.get("tool_choice")?;
    let choice = match value {
        Value::String(value) => match value.as_str() {
            "auto" => CanonicalToolChoice::Auto,
            "required" | "any" => CanonicalToolChoice::Required,
            "none" => CanonicalToolChoice::None,
            _ => return None,
        },
        Value::Object(choice) => {
            let kind = choice.get("type").and_then(Value::as_str)?;
            let (projected, allowed): (CanonicalToolChoice, &[&str]) = match kind {
                "auto" => (CanonicalToolChoice::Auto, &["type"]),
                "any" | "required" => (CanonicalToolChoice::Required, &["type"]),
                "none" => (CanonicalToolChoice::None, &["type"]),
                "tool" => (
                    CanonicalToolChoice::Named(
                        choice.get("name").and_then(Value::as_str)?.to_owned(),
                    ),
                    &["type", "name"],
                ),
                "function" if choice.contains_key("name") => (
                    CanonicalToolChoice::Named(
                        choice.get("name").and_then(Value::as_str)?.to_owned(),
                    ),
                    &["type", "name"],
                ),
                "function" => (
                    CanonicalToolChoice::Named(
                        choice
                            .get("function")?
                            .get("name")
                            .and_then(Value::as_str)?
                            .to_owned(),
                    ),
                    &["type", "function"],
                ),
                _ => return None,
            };
            if choice
                .keys()
                .any(|field| !allowed.contains(&field.as_str()))
                || choice
                    .get("function")
                    .and_then(Value::as_object)
                    .is_some_and(|function| function.keys().any(|field| field.as_str() != "name"))
            {
                return None;
            }
            projected
        }
        _ => return None,
    };
    let valid_shape = match operation {
        GatewayOperation::AnthropicMessages | GatewayOperation::AnthropicCountTokens => {
            value.is_object()
        }
        GatewayOperation::OpenAiResponses | GatewayOperation::OpenAiChatCompletions => true,
        GatewayOperation::GeminiGenerateContent => false,
    };
    if !valid_shape {
        return None;
    }
    object.remove("tool_choice");
    Some(choice)
}

fn take_reasoning(
    operation: GatewayOperation,
    object: &mut Map<String, Value>,
) -> Option<CanonicalReasoning> {
    if operation == GatewayOperation::GeminiGenerateContent {
        return None;
    }
    if matches!(operation, GatewayOperation::OpenAiChatCompletions)
        && let Some(effort) = object.get("reasoning_effort").and_then(Value::as_str)
    {
        let reasoning = CanonicalReasoning {
            effort: Some(effort.to_owned()),
            ..CanonicalReasoning::default()
        };
        object.remove("reasoning_effort");
        return Some(reasoning);
    }
    let key = match operation {
        GatewayOperation::AnthropicMessages | GatewayOperation::AnthropicCountTokens => "thinking",
        GatewayOperation::OpenAiResponses | GatewayOperation::OpenAiChatCompletions => "reasoning",
        GatewayOperation::GeminiGenerateContent => return None,
    };
    let source = object.get(key)?.as_object()?;
    let allowed = match operation {
        GatewayOperation::AnthropicMessages | GatewayOperation::AnthropicCountTokens => {
            ["type", "budget_tokens"].as_slice()
        }
        GatewayOperation::OpenAiResponses | GatewayOperation::OpenAiChatCompletions => {
            ["effort", "summary"].as_slice()
        }
        GatewayOperation::GeminiGenerateContent => return None,
    };
    if source
        .keys()
        .any(|field| !allowed.contains(&field.as_str()))
    {
        return None;
    }
    let reasoning = match operation {
        GatewayOperation::AnthropicMessages | GatewayOperation::AnthropicCountTokens => {
            let enabled = match source.get("type").and_then(Value::as_str) {
                Some("enabled") => Some(true),
                Some("adaptive") => Some(true),
                Some("disabled") => Some(false),
                _ => return None,
            };
            let budget_tokens = source.get("budget_tokens").and_then(Value::as_u64);
            if source.contains_key("budget_tokens") && budget_tokens.is_none() {
                return None;
            }
            CanonicalReasoning {
                enabled,
                budget_tokens,
                ..CanonicalReasoning::default()
            }
        }
        GatewayOperation::OpenAiResponses | GatewayOperation::OpenAiChatCompletions => {
            let effort = optional_string(source, "effort")?;
            let summary = optional_string(source, "summary")?;
            CanonicalReasoning {
                effort,
                summary,
                ..CanonicalReasoning::default()
            }
        }
        GatewayOperation::GeminiGenerateContent => return None,
    };
    object.remove(key);
    Some(reasoning)
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    match object.get(key) {
        None => Some(None),
        Some(Value::String(value)) => Some(Some(value.clone())),
        Some(_) => None,
    }
}

fn take_structured_output(
    operation: GatewayOperation,
    object: &mut Map<String, Value>,
) -> Option<CanonicalStructuredOutput> {
    match operation {
        GatewayOperation::OpenAiChatCompletions => {
            let value = object.get("response_format")?;
            let format = parse_structured_output(value)?;
            object.remove("response_format");
            Some(format)
        }
        GatewayOperation::OpenAiResponses => {
            take_nested_structured_output(object, "text", "format")
        }
        GatewayOperation::AnthropicMessages | GatewayOperation::AnthropicCountTokens => {
            take_nested_structured_output(object, "output_config", "format")
        }
        GatewayOperation::GeminiGenerateContent => None,
    }
}

fn take_nested_structured_output(
    object: &mut Map<String, Value>,
    outer_key: &str,
    inner_key: &str,
) -> Option<CanonicalStructuredOutput> {
    let value = object.get(outer_key)?.get(inner_key)?;
    let format = parse_structured_output(value)?;
    let outer = object.get_mut(outer_key)?.as_object_mut()?;
    outer.remove(inner_key);
    if outer.is_empty() {
        object.remove(outer_key);
    }
    Some(format)
}

fn parse_structured_output(value: &Value) -> Option<CanonicalStructuredOutput> {
    let outer = value.as_object()?;
    let kind = outer.get("type").and_then(Value::as_str)?;
    if kind == "json_object" {
        if outer.keys().any(|field| field != "type") {
            return None;
        }
        return Some(CanonicalStructuredOutput {
            kind: CanonicalStructuredOutputKind::JsonObject,
            name: None,
            description: None,
            schema: Value::Object(Map::new()),
            strict: None,
        });
    }
    if kind != "json_schema" {
        return None;
    }
    let nested_definition = outer.contains_key("json_schema");
    let definition = outer
        .get("json_schema")
        .and_then(Value::as_object)
        .unwrap_or(outer);
    let outer_allowed: &[&str] = if nested_definition {
        &["type", "json_schema"]
    } else {
        &["type", "name", "description", "schema", "strict"]
    };
    if outer
        .keys()
        .any(|field| !outer_allowed.contains(&field.as_str()))
        || definition.keys().any(|field| {
            !["name", "description", "schema", "strict"].contains(&field.as_str())
                && !(!nested_definition && field == "type")
        })
    {
        return None;
    }
    Some(CanonicalStructuredOutput {
        kind: CanonicalStructuredOutputKind::JsonSchema,
        name: optional_string(definition, "name")?,
        description: optional_string(definition, "description")?,
        schema: definition.get("schema")?.clone(),
        strict: match definition.get("strict") {
            None => None,
            Some(Value::Bool(value)) => Some(*value),
            Some(_) => return None,
        },
    })
}

fn take_tools(
    operation: GatewayOperation,
    object: &mut Map<String, Value>,
) -> Vec<CanonicalToolDefinition> {
    let provider = source_provider(operation);
    let Some(values) = object.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    let projected = values
        .iter()
        .map(|value| project_tool_definition(provider, value))
        .collect::<Option<Vec<_>>>();
    match projected {
        Some(tools) => {
            object.remove("tools");
            tools
        }
        None => Vec::new(),
    }
}

fn project_tool_definition(provider: &str, value: &Value) -> Option<CanonicalToolDefinition> {
    let source = value.as_object()?;
    let nested = provider == "openai" && source.get("function").is_some_and(Value::is_object);
    let definition_source = if nested {
        source.get("function")?.as_object()?
    } else {
        source
    };
    let name = definition_source.get("name")?.as_str()?.to_owned();
    let description = optional_string(definition_source, "description")?;
    let schema_keys = ["input_schema", "parameters"]
        .into_iter()
        .filter(|key| definition_source.contains_key(*key))
        .collect::<Vec<_>>();
    if schema_keys.len() > 1 {
        return None;
    }
    let input_schema = schema_keys
        .first()
        .and_then(|key| definition_source.get(*key))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let strict = match definition_source.get("strict") {
        None => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return None,
    };
    let mut definition = definition_source.clone();
    definition.remove("name");
    if description.is_some() {
        definition.remove("description");
    }
    if let Some(key) = schema_keys.first() {
        definition.remove(*key);
    }
    if strict.is_some() {
        definition.remove("strict");
    }
    let mut outer = if nested {
        source.clone()
    } else {
        definition.clone()
    };

    if nested {
        outer.remove("function");
        if outer.get("type").and_then(Value::as_str) == Some("function") {
            outer.remove("type");
        }
        if !definition.is_empty() {
            outer.insert("function".to_owned(), Value::Object(definition));
        }
    } else if outer.get("type").and_then(Value::as_str) == Some("function") {
        outer.remove("type");
    }

    Some(CanonicalToolDefinition {
        name,
        description,
        input_schema,
        strict,
        provider_options: scoped_options(provider, outer),
    })
}

fn take_messages(
    operation: GatewayOperation,
    object: &mut Map<String, Value>,
) -> Vec<CanonicalMessage> {
    let provider = source_provider(operation);
    let mut messages = Vec::new();
    let system_key = match operation {
        GatewayOperation::AnthropicMessages | GatewayOperation::AnthropicCountTokens => "system",
        GatewayOperation::OpenAiResponses => "instructions",
        GatewayOperation::OpenAiChatCompletions => "",
        GatewayOperation::GeminiGenerateContent => "",
    };
    if !system_key.is_empty()
        && let Some(system) = object.get(system_key)
        && let Some(content) = project_content(provider, system)
    {
        messages.push(CanonicalMessage {
            role: CanonicalRole::System,
            name: None,
            content,
            provider_options: ProviderOptions::new(),
        });
        object.remove(system_key);
    }

    let content_key = match operation {
        GatewayOperation::AnthropicMessages
        | GatewayOperation::AnthropicCountTokens
        | GatewayOperation::OpenAiChatCompletions => "messages",
        GatewayOperation::OpenAiResponses => "input",
        GatewayOperation::GeminiGenerateContent => return messages,
    };
    let Some(value) = object.get(content_key) else {
        return messages;
    };
    let projected = match value {
        Value::String(_) => project_content(provider, value).map(|content| {
            vec![CanonicalMessage {
                role: CanonicalRole::User,
                name: None,
                content,
                provider_options: ProviderOptions::new(),
            }]
        }),
        Value::Array(values) => Some(
            values
                .iter()
                .map(|value| project_message(provider, value, CanonicalRole::User))
                .collect(),
        ),
        _ => None,
    };
    if let Some(mut projected) = projected {
        messages.append(&mut projected);
        object.remove(content_key);
    }
    messages
}

fn project_message(provider: &str, value: &Value, default_role: CanonicalRole) -> CanonicalMessage {
    let Some(source) = value.as_object() else {
        return opaque_message(provider, value);
    };
    if source.get("role").is_some_and(|value| !value.is_string())
        || source.get("name").is_some_and(|value| !value.is_string())
    {
        return opaque_message(provider, value);
    }
    let mut residual = source.clone();
    if matches!(
        source.get("type").and_then(Value::as_str),
        Some("function_call")
    ) {
        let Some(call) = project_tool_call(provider, value) else {
            return opaque_message(provider, value);
        };
        return CanonicalMessage {
            role: CanonicalRole::Assistant,
            name: None,
            content: vec![CanonicalContent::ToolCall(call)],
            provider_options: ProviderOptions::new(),
        };
    }
    if matches!(
        source.get("type").and_then(Value::as_str),
        Some("function_call_output")
    ) {
        let content = project_content_item(provider, value);
        if matches!(content, CanonicalContent::Opaque { .. }) {
            return opaque_message(provider, value);
        }
        return CanonicalMessage {
            role: CanonicalRole::Tool,
            name: None,
            content: vec![content],
            provider_options: ProviderOptions::new(),
        };
    }
    if source.get("type").and_then(Value::as_str) == Some("message") {
        residual.remove("type");
    }
    let role = source
        .get("role")
        .and_then(Value::as_str)
        .map(CanonicalRole::from_wire)
        .unwrap_or(default_role);
    if matches!(role, CanonicalRole::Tool)
        && source
            .get("tool_call_id")
            .is_some_and(|value| !value.is_string())
    {
        return opaque_message(provider, value);
    }
    if source.get("role").is_some_and(Value::is_string) {
        residual.remove("role");
    }
    let name = source
        .get("name")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    if name.is_some() {
        residual.remove("name");
    }
    let content_value = residual.remove("content");
    let tool_call_id = matches!(role, CanonicalRole::Tool)
        .then(|| {
            source
                .get("tool_call_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .flatten();
    if tool_call_id.is_some() {
        residual.remove("tool_call_id");
    }
    let mut content = if let Some(tool_call_id) = tool_call_id {
        vec![CanonicalContent::ToolResult {
            tool_call_id,
            content: content_value.unwrap_or(Value::Null),
            provider_options: ProviderOptions::new(),
        }]
    } else {
        content_value
            .and_then(|value| project_content(provider, &value))
            .unwrap_or_default()
    };
    if residual.get("tool_calls").is_some_and(Value::is_array)
        && let Some(tool_calls) = residual.remove("tool_calls")
    {
        content.extend(project_tool_calls(provider, &tool_calls));
    }
    if content.is_empty() {
        return opaque_message(provider, value);
    }
    CanonicalMessage {
        role,
        name,
        content,
        provider_options: scoped_options(provider, residual),
    }
}

fn opaque_message(provider: &str, value: &Value) -> CanonicalMessage {
    CanonicalMessage {
        role: CanonicalRole::Other("opaque".to_owned()),
        name: None,
        content: vec![opaque(provider, value)],
        provider_options: ProviderOptions::new(),
    }
}

fn project_content(provider: &str, value: &Value) -> Option<Vec<CanonicalContent>> {
    match value {
        Value::String(text) => Some(vec![CanonicalContent::Text {
            text: text.clone(),
            provider_options: ProviderOptions::new(),
        }]),
        Value::Array(values) => Some(
            values
                .iter()
                .map(|value| project_content_item(provider, value))
                .collect(),
        ),
        Value::Object(_) => Some(vec![project_content_item(provider, value)]),
        _ => None,
    }
}

fn project_content_item(provider: &str, value: &Value) -> CanonicalContent {
    let Some(source) = value.as_object() else {
        return CanonicalContent::Opaque {
            provider: provider.to_owned(),
            value: value.clone(),
        };
    };
    let mut residual = source.clone();
    let kind = residual
        .remove("type")
        .and_then(|value| value.as_str().map(ToOwned::to_owned));
    match kind.as_deref() {
        Some("text" | "input_text" | "output_text") => residual
            .remove("text")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .map_or_else(
                || opaque(provider, value),
                |text| CanonicalContent::Text {
                    text,
                    provider_options: scoped_options(provider, residual),
                },
            ),
        Some("thinking" | "reasoning" | "summary_text") => {
            (!aliases_conflict(&residual, &["thinking", "text"]))
                .then_some(())
                .and_then(|_| {
                    residual
                        .remove("thinking")
                        .or_else(|| residual.remove("text"))
                })
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .map_or_else(
                    || opaque(provider, value),
                    |text| CanonicalContent::Reasoning {
                        text,
                        provider_options: scoped_options(provider, residual),
                    },
                )
        }
        Some("image" | "image_url" | "input_image") => {
            (!aliases_conflict(&residual, &["source", "image_url"]))
                .then_some(())
                .and_then(|_| {
                    residual
                        .remove("source")
                        .or_else(|| residual.remove("image_url"))
                })
                .map_or_else(
                    || opaque(provider, value),
                    |source| CanonicalContent::Image {
                        source,
                        provider_options: scoped_options(provider, residual),
                    },
                )
        }
        Some("tool_use" | "function_call") => project_tool_call(provider, value)
            .map(CanonicalContent::ToolCall)
            .unwrap_or_else(|| opaque(provider, value)),
        Some("tool_result" | "function_call_output") => {
            if aliases_conflict(&residual, &["tool_use_id", "call_id"])
                || aliases_conflict(&residual, &["content", "output"])
            {
                return opaque(provider, value);
            }
            let id = residual
                .remove("tool_use_id")
                .or_else(|| residual.remove("call_id"))
                .and_then(|value| value.as_str().map(ToOwned::to_owned));
            let content = residual
                .remove("content")
                .or_else(|| residual.remove("output"));
            match (id, content) {
                (Some(tool_call_id), Some(content)) => CanonicalContent::ToolResult {
                    tool_call_id,
                    content,
                    provider_options: scoped_options(provider, residual),
                },
                _ => opaque(provider, value),
            }
        }
        _ => opaque(provider, value),
    }
}

fn aliases_conflict(object: &Map<String, Value>, aliases: &[&str]) -> bool {
    aliases
        .iter()
        .filter(|field| object.contains_key(**field))
        .count()
        > 1
}

fn project_tool_calls(provider: &str, value: &Value) -> Vec<CanonicalContent> {
    value
        .as_array()
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    project_tool_call(provider, value)
                        .map(CanonicalContent::ToolCall)
                        .unwrap_or_else(|| opaque(provider, value))
                })
                .collect()
        })
        .unwrap_or_else(|| vec![opaque(provider, value)])
}

fn project_tool_call(provider: &str, value: &Value) -> Option<CanonicalToolCall> {
    let source = value.as_object()?;
    let mut outer = source.clone();
    let nested = provider == "openai" && outer.get("function").is_some_and(Value::is_object);
    let responses_item = provider == "openai"
        && !nested
        && outer.get("type").and_then(Value::as_str) == Some("function_call");
    let mut call = if nested {
        outer.remove("function")?.as_object()?.clone()
    } else {
        outer.clone()
    };
    if responses_item {
        if outer
            .get("status")
            .is_some_and(|status| status.as_str() != Some("completed"))
        {
            return None;
        }
        outer.remove("id");
        outer.remove("status");
        call.remove("id");
        call.remove("status");
    }
    let id_keys: &[&str] = if responses_item {
        &["call_id"]
    } else {
        &["id", "call_id"]
    };
    let id_keys = id_keys
        .iter()
        .copied()
        .filter(|key| source.contains_key(*key))
        .collect::<Vec<_>>();
    if id_keys.len() > 1 {
        return None;
    }
    let id = match id_keys.first().and_then(|key| source.get(*key)) {
        Some(value) => value.as_str()?.to_owned(),
        None => String::new(),
    };
    if let Some(key) = id_keys.first() {
        outer.remove(*key);
        call.remove(*key);
    }
    let name = call.get("name")?.as_str()?.to_owned();
    call.remove("name");
    let argument_keys = ["input", "arguments"]
        .into_iter()
        .filter(|key| call.contains_key(*key))
        .collect::<Vec<_>>();
    if argument_keys.len() > 1 {
        return None;
    }
    let arguments = argument_keys
        .first()
        .and_then(|key| call.get(*key))
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(key) = argument_keys.first() {
        call.remove(*key);
    }
    if outer.get("type").and_then(Value::as_str) == Some("function")
        || outer.get("type").and_then(Value::as_str) == Some("function_call")
        || outer.get("type").and_then(Value::as_str) == Some("tool_use")
    {
        outer.remove("type");
    }
    if matches!(
        call.get("type").and_then(Value::as_str),
        Some("function" | "function_call" | "tool_use")
    ) {
        call.remove("type");
    }
    if nested {
        if !call.is_empty() {
            outer.insert("function".to_owned(), Value::Object(call));
        }
    } else {
        outer = call;
    }
    Some(CanonicalToolCall {
        id,
        name,
        arguments: CanonicalToolArguments::Complete(arguments),
        provider_options: scoped_options(provider, outer),
    })
}

fn opaque(provider: &str, value: &Value) -> CanonicalContent {
    CanonicalContent::Opaque {
        provider: provider.to_owned(),
        value: value.clone(),
    }
}

pub(crate) struct ChatExtras {
    pub temperature: Option<Value>,
    pub include_usage: bool,
}

pub(crate) fn chat_extras(options: &ProviderOptions) -> Option<ChatExtras> {
    if options.len() != 1 {
        return None;
    }
    let object = options.get("openai")?.as_object()?;
    let mut extras = ChatExtras {
        temperature: None,
        include_usage: false,
    };
    for (key, value) in object {
        match key.as_str() {
            "temperature" => {
                if !value
                    .as_f64()
                    .is_some_and(|number| (0.0..=1.0).contains(&number))
                {
                    return None;
                }
                extras.temperature = Some(value.clone());
            }
            "n" => {
                if value.as_u64() != Some(1) {
                    return None;
                }
            }
            "stream_options" => {
                let stream_options = value.as_object()?;
                if stream_options.keys().any(|field| field != "include_usage") {
                    return None;
                }
                if let Some(flag) = stream_options.get("include_usage") {
                    extras.include_usage = flag.as_bool()?;
                }
            }
            _ => return None,
        }
    }
    Some(extras)
}

fn scoped_options(provider: &str, residual: Map<String, Value>) -> ProviderOptions {
    if residual.is_empty() {
        ProviderOptions::new()
    } else {
        ProviderOptions::from([(provider.to_owned(), Value::Object(residual))])
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Bytes;
    use serde_json::{Value, json};

    use super::{DecodedProtocolRequest, decode};
    use crate::{
        canonical::{CanonicalContent, CanonicalRole, CanonicalToolArguments, CanonicalToolChoice},
        model_registry::GatewayOperation,
    };

    #[test]
    fn anthropic_projection_keeps_scoped_native_options() {
        let request = decode(
            GatewayOperation::AnthropicMessages,
            Bytes::from_static(br#"{"model":"quality:premium","system":[{"type":"text","text":"rules","cache_control":{"type":"ephemeral"}}],"messages":[{"role":"user","content":[{"type":"text","text":"hello","citations":{"enabled":true}}]}],"tools":[{"name":"lookup","description":"find","input_schema":{"type":"object"},"cache_control":{"type":"ephemeral"}}],"tool_choice":{"type":"auto"},"stream":true,"max_tokens":321,"temperature":0.2,"vendor_number":123456789012345678901234567890}"#),
        )
        .unwrap();

        assert_eq!(request.requested_model, "quality:premium");
        assert_eq!(request.requested_quality.as_deref(), Some("premium"));
        assert!(request.stream);
        assert_eq!(request.max_output_tokens, Some(321));
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, CanonicalRole::System);
        assert_eq!(request.messages[1].role, CanonicalRole::User);
        let CanonicalContent::Text {
            text,
            provider_options,
        } = &request.messages[0].content[0]
        else {
            panic!("system block was not canonical text");
        };
        assert_eq!(text, "rules");
        assert_eq!(
            provider_options["anthropic"]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(request.tools[0].name, "lookup");
        assert_eq!(
            request.tools[0].provider_options["anthropic"]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(request.tool_choice, Some(CanonicalToolChoice::Auto));
        assert_vendor_extension(&request);
    }

    #[test]
    fn dropping_a_native_field_breaks_the_projection_golden_check() {
        let mut request = decode(
            GatewayOperation::AnthropicMessages,
            Bytes::from_static(
                br#"{"model":"public","messages":[],"temperature":0.2,"vendor_number":7}"#,
            ),
        )
        .unwrap();
        assert!(has_vendor_extension(&request));
        request
            .provider_options
            .get_mut("anthropic")
            .and_then(Value::as_object_mut)
            .and_then(|options| options.remove("vendor_number"));
        assert!(!has_vendor_extension(&request));
    }

    #[test]
    fn dropping_a_nested_native_field_breaks_the_projection_golden_check() {
        let mut request = decode(
            GatewayOperation::AnthropicMessages,
            Bytes::from_static(br#"{"model":"public","messages":[{"role":"user","content":[{"type":"text","text":"hello","citations":{"enabled":true}}]}]}"#),
        )
        .unwrap();
        assert!(has_nested_citations(&request));
        let CanonicalContent::Text {
            provider_options, ..
        } = &mut request.messages[0].content[0]
        else {
            panic!("canonical projection lost nested vendor field");
        };
        provider_options.clear();
        assert!(!has_nested_citations(&request));
    }

    #[test]
    fn openai_responses_projection_keeps_instructions_tools_and_options() {
        let request = decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from_static(br#"{"model":"public","instructions":"be concise","input":"hello","tools":[{"type":"function","name":"lookup","description":"find","parameters":{"type":"object"},"strict":true}],"tool_choice":"auto","stream":true,"max_output_tokens":77,"reasoning":{"effort":"high"}}"#),
        )
        .unwrap();

        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, CanonicalRole::System);
        assert_eq!(request.messages[1].role, CanonicalRole::User);
        assert_eq!(request.tools[0].name, "lookup");
        assert_eq!(request.tools[0].input_schema, json!({"type": "object"}));
        assert_eq!(request.tools[0].strict, Some(true));
        assert!(request.tools[0].provider_options.is_empty());
        assert_eq!(
            request
                .reasoning
                .as_ref()
                .and_then(|value| value.effort.as_deref()),
            Some("high")
        );
        assert_eq!(request.tool_choice, Some(CanonicalToolChoice::Auto));
        assert_eq!(request.max_output_tokens, Some(77));
    }

    #[test]
    fn generated_responses_function_call_can_be_used_as_the_next_input() {
        let request = decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from_static(
                br#"{"model":"public","input":[{"type":"function_call","id":"fc_public_0","call_id":"call_1","name":"lookup","arguments":"{\"q\":\"x\"}","status":"completed"},{"type":"function_call_output","call_id":"call_1","output":"found"}],"max_output_tokens":8}"#,
            ),
        )
        .unwrap();
        let CanonicalContent::ToolCall(call) = &request.messages[0].content[0] else {
            panic!("generated function call was not projected");
        };
        assert_eq!(call.id, "call_1");
        assert!(call.provider_options.is_empty());
        assert!(matches!(
            call.arguments,
            CanonicalToolArguments::Complete(Value::String(ref value))
                if value == "{\"q\":\"x\"}"
        ));
    }

    #[test]
    fn structured_output_is_provider_neutral() {
        let request = decode(
            GatewayOperation::OpenAiChatCompletions,
            Bytes::from_static(br#"{"model":"public","messages":[],"response_format":{"type":"json_schema","json_schema":{"name":"answer","schema":{"type":"object"},"strict":true}}}"#),
        )
        .unwrap();
        let output = request.structured_output.expect("structured output");
        assert_eq!(
            output.kind,
            crate::canonical::CanonicalStructuredOutputKind::JsonSchema
        );
        assert_eq!(output.name.as_deref(), Some("answer"));
        assert_eq!(output.schema, json!({"type": "object"}));
        assert_eq!(output.strict, Some(true));
    }

    #[test]
    fn conflicting_output_aliases_remain_explicit_and_unconsumed() {
        let request = decode(
            GatewayOperation::OpenAiChatCompletions,
            Bytes::from_static(
                br#"{"model":"public","messages":[],"max_completion_tokens":20,"max_tokens":10}"#,
            ),
        )
        .unwrap();
        assert_eq!(request.max_output_tokens, None);
        assert_eq!(
            request.metadata["projection_conflicts"],
            json!(["multiple_output_limit_aliases"])
        );
        assert_eq!(
            request.provider_options["openai"]["max_completion_tokens"],
            20
        );
        assert_eq!(request.provider_options["openai"]["max_tokens"], 10);
    }

    #[test]
    fn malformed_accepted_nodes_are_preserved_as_opaque() {
        let original = json!({
            "type": "function_call",
            "call_id": "call_1",
            "arguments": "{}"
        });
        let request = decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from(
                serde_json::to_vec(&json!({
                    "model": "public",
                    "input": [original.clone()]
                }))
                .unwrap(),
            ),
        )
        .unwrap();
        let CanonicalContent::Opaque { value, .. } = &request.messages[0].content[0] else {
            panic!("malformed function call was not kept opaque");
        };
        assert_eq!(value, &original);
    }

    #[test]
    fn conflicting_reasoning_and_content_aliases_do_not_create_two_authorities() {
        let request = decode(
            GatewayOperation::OpenAiChatCompletions,
            Bytes::from_static(br#"{"model":"public","messages":[],"reasoning_effort":"high","reasoning":{"effort":"low"}}"#),
        )
        .unwrap();
        assert!(request.reasoning.is_none());
        assert_eq!(
            request.metadata["projection_conflicts"],
            json!(["multiple_reasoning_aliases"])
        );
        assert_eq!(
            request.provider_options["openai"]["reasoning_effort"],
            "high"
        );
        assert_eq!(
            request.provider_options["openai"]["reasoning"]["effort"],
            "low"
        );

        let content = json!({
            "type": "reasoning",
            "thinking": "one",
            "text": "two"
        });
        let request = decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from(
                serde_json::to_vec(&json!({"model": "public", "input": [content.clone()]}))
                    .unwrap(),
            ),
        )
        .unwrap();
        let CanonicalContent::Opaque { value, .. } = &request.messages[0].content[0] else {
            panic!("conflicting content aliases were interpreted");
        };
        assert_eq!(value, &content);
    }

    #[test]
    fn json_object_and_json_schema_are_distinct() {
        let request = decode(
            GatewayOperation::OpenAiChatCompletions,
            Bytes::from_static(
                br#"{"model":"public","messages":[],"response_format":{"type":"json_object"}}"#,
            ),
        )
        .unwrap();
        assert_eq!(
            request.structured_output.expect("json object").kind,
            crate::canonical::CanonicalStructuredOutputKind::JsonObject
        );
    }

    #[test]
    fn openai_chat_projection_keeps_tool_call_arguments() {
        let request = decode(
            GatewayOperation::OpenAiChatCompletions,
            Bytes::from_static(br#"{"model":"public","messages":[{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"x\"}"}}]}],"stream":true,"max_completion_tokens":55}"#),
        )
        .unwrap();

        assert_eq!(request.messages.len(), 1);
        let CanonicalContent::ToolCall(call) = &request.messages[0].content[0] else {
            panic!("chat tool call was not projected");
        };
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "lookup");
        assert_eq!(
            call.arguments,
            CanonicalToolArguments::Complete(Value::String("{\"q\":\"x\"}".to_owned()))
        );
        assert_eq!(request.max_output_tokens, Some(55));
    }

    #[test]
    fn anthropic_count_tokens_has_a_zero_output_limit() {
        let request = decode(
            GatewayOperation::AnthropicCountTokens,
            Bytes::from_static(br#"{"model":"public","messages":[]}"#),
        )
        .unwrap();
        assert_eq!(request.max_output_tokens, Some(0));
        assert!(!request.stream);
    }

    #[test]
    fn anthropic_adaptive_thinking_is_preserved_for_translation() {
        let request = decode(
            GatewayOperation::AnthropicMessages,
            Bytes::from_static(
                br#"{"model":"public","messages":[],"thinking":{"type":"adaptive"}}"#,
            ),
        )
        .unwrap();

        assert_eq!(request.reasoning.unwrap().enabled, Some(true));
    }

    fn assert_vendor_extension(request: &DecodedProtocolRequest) {
        assert!(
            has_vendor_extension(request),
            "canonical projection lost vendor field"
        );
    }

    fn has_vendor_extension(request: &DecodedProtocolRequest) -> bool {
        let options = request
            .provider_options
            .get("anthropic")
            .and_then(Value::as_object);
        options.is_some_and(|options| options.contains_key("vendor_number"))
    }

    fn has_nested_citations(request: &DecodedProtocolRequest) -> bool {
        request
            .messages
            .first()
            .and_then(|message| message.content.first())
            .and_then(|content| match content {
                CanonicalContent::Text {
                    provider_options, ..
                } => provider_options
                    .get("anthropic")
                    .and_then(|value| value.get("citations")),
                _ => None,
            })
            .is_some()
    }
}
