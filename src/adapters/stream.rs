use std::collections::{BTreeMap, HashMap, HashSet};

use axum::body::Bytes;
use serde_json::{Value, json};

use crate::{
    adapters::response::openai_response_envelope,
    canonical::{
        CanonicalContent, CanonicalError, CanonicalRequest, CanonicalRole, CanonicalStreamEvent,
        CanonicalToolArguments, CanonicalToolCall, CanonicalUsage, ProviderOptions,
    },
    model_registry::{GatewayOperation, ProviderKind},
    services::streaming::{ProtocolError, StreamEncoder, StreamObserver, event_data},
};

const MAX_BLOCKS: usize = 4096;
const MAX_TOOL_ARGUMENT_BYTES: usize = 8 * 1024 * 1024;

pub enum CrossStreamObserver {
    Anthropic(AnthropicObserver),
    OpenAi(OpenAiResponsesObserver),
    Gemini(GeminiObserver),
}

impl CrossStreamObserver {
    pub fn new(operation: GatewayOperation) -> Result<Self, ProtocolError> {
        match operation {
            GatewayOperation::AnthropicMessages => {
                Ok(Self::Anthropic(AnthropicObserver::default()))
            }
            GatewayOperation::OpenAiResponses => {
                Ok(Self::OpenAi(OpenAiResponsesObserver::default()))
            }
            GatewayOperation::GeminiGenerateContent => Ok(Self::Gemini(GeminiObserver::default())),
            _ => Err(ProtocolError::Invalid(
                "cross-provider stream target is unsupported",
            )),
        }
    }
}

impl StreamObserver for CrossStreamObserver {
    fn observe(&mut self, event: &[u8]) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
        match self {
            Self::Anthropic(observer) => observer.observe(event),
            Self::OpenAi(observer) => observer.observe(event),
            Self::Gemini(observer) => observer.observe(event),
        }
    }

    fn finish(&mut self) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
        Err(ProtocolError::Invalid(
            "translated provider stream ended without its terminal event",
        ))
    }
}

#[derive(Default)]
pub struct GeminiObserver {
    started: bool,
    content_started: bool,
    terminal: bool,
}

impl StreamObserver for GeminiObserver {
    fn observe(&mut self, event: &[u8]) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
        let value = strict_event(event)?;
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        if self.terminal {
            return Err(ProtocolError::Invalid(
                "Gemini event followed the terminal event",
            ));
        }
        let response = super::gemini_wire::decode(value.clone())
            .map_err(|_| ProtocolError::Invalid("Gemini chunk is invalid"))?;
        if response.error.is_some() {
            self.terminal = true;
            return Ok(vec![CanonicalStreamEvent::Error(CanonicalError::provider(
                "provider_stream_error",
            ))]);
        }
        let candidate = response
            .candidates
            .first()
            .ok_or(ProtocolError::Invalid("Gemini chunk has no candidate"))?;
        let mut events = Vec::new();
        if !self.started {
            self.started = true;
            events.extend([
                CanonicalStreamEvent::ResponseStarted("gemini".to_owned()),
                CanonicalStreamEvent::MessageStarted {
                    role: CanonicalRole::Assistant,
                },
            ]);
        }
        for part in &candidate.content.parts {
            let text = part
                .text
                .as_deref()
                .filter(|_| part.function_call.is_none())
                .ok_or(ProtocolError::Invalid("Gemini stream part is not text"))?;
            if !self.content_started {
                self.content_started = true;
                events.push(CanonicalStreamEvent::ContentBlockStarted {
                    index: 0,
                    content: CanonicalContent::Text {
                        text: String::new(),
                        provider_options: ProviderOptions::new(),
                    },
                });
            }
            events.push(CanonicalStreamEvent::TextDelta {
                index: 0,
                text: text.to_owned(),
            });
        }
        if let Some(reason) = candidate.finish_reason.as_deref() {
            let finish_reason = match reason {
                "STOP" => "stop",
                "MAX_TOKENS" => "max_tokens",
                _ => {
                    return Err(ProtocolError::Invalid(
                        "Gemini finish reason is not translatable",
                    ));
                }
            };
            if self.content_started {
                events.push(CanonicalStreamEvent::ContentBlockCompleted { index: 0 });
            }
            let encoded = serde_json::to_vec(&value)
                .map_err(|_| ProtocolError::Invalid("Gemini usage could not be encoded"))?;
            let usage = super::provider::usage_for(ProviderKind::Gemini, &encoded)
                .map_err(|_| ProtocolError::Invalid("Gemini terminal usage is invalid"))?
                .ok_or(ProtocolError::Invalid("Gemini terminal usage is missing"))?;
            events.extend([
                CanonicalStreamEvent::OutputCompleted {
                    index: 0,
                    finish_reason: finish_reason.to_owned(),
                },
                CanonicalStreamEvent::Usage(usage),
                CanonicalStreamEvent::MessageCompleted,
                CanonicalStreamEvent::ResponseCompleted,
            ]);
            self.terminal = true;
        }
        Ok(events)
    }
}

pub struct CrossStreamEncoder {
    inner: OpenAiResponsesEncoder,
}

impl CrossStreamEncoder {
    pub fn new(
        operation: GatewayOperation,
        response_id: String,
        public_model: String,
        source_request: &CanonicalRequest,
    ) -> Result<Self, ProtocolError> {
        match operation {
            GatewayOperation::OpenAiResponses => Ok(Self {
                inner: OpenAiResponsesEncoder::new(response_id, public_model, source_request),
            }),
            _ => Err(ProtocolError::Invalid(
                "cross-provider stream source is unsupported",
            )),
        }
    }
}

impl StreamEncoder for CrossStreamEncoder {
    fn encode(&mut self, events: &[CanonicalStreamEvent]) -> Result<Vec<Bytes>, ProtocolError> {
        self.inner.encode(events)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BlockKind {
    Text,
    Tool,
}

#[derive(Default)]
pub struct AnthropicObserver {
    started: bool,
    message_delta: bool,
    terminal: bool,
    blocks: HashMap<u32, BlockKind>,
    completed: HashSet<u32>,
    usage: CanonicalUsage,
    tool_argument_bytes: usize,
    tool_arguments: HashMap<u32, String>,
    tool_ids: HashSet<String>,
    finish_reason: Option<String>,
}

impl StreamObserver for AnthropicObserver {
    fn observe(&mut self, event: &[u8]) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
        let value = strict_event(event)?;
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let kind = required_str(&value, "type")?;
        if self.terminal {
            return Err(ProtocolError::Invalid(
                "Anthropic event followed the terminal event",
            ));
        }
        match kind {
            "ping" => Ok(Vec::new()),
            "message_start" => {
                if self.started {
                    return Err(ProtocolError::Invalid("duplicate Anthropic message_start"));
                }
                let message = value
                    .get("message")
                    .ok_or(ProtocolError::Invalid("message_start has no message"))?;
                if message.get("type").and_then(Value::as_str) != Some("message")
                    || message.get("role").and_then(Value::as_str) != Some("assistant")
                {
                    return Err(ProtocolError::Invalid(
                        "Anthropic message_start envelope is invalid",
                    ));
                }
                let id = required_str(message, "id")?.to_owned();
                self.started = true;
                self.usage = anthropic_initial_usage(message.get("usage"))?;
                Ok(vec![
                    CanonicalStreamEvent::ResponseStarted(id),
                    CanonicalStreamEvent::MessageStarted {
                        role: CanonicalRole::Assistant,
                    },
                    CanonicalStreamEvent::Usage(self.usage.clone()),
                ])
            }
            "content_block_start" => {
                self.require_started()?;
                if self.message_delta {
                    return Err(ProtocolError::Invalid(
                        "Anthropic content followed message_delta",
                    ));
                }
                let index = required_index(&value)?;
                if self.blocks.len() >= MAX_BLOCKS
                    || self.blocks.contains_key(&index)
                    || usize::try_from(index).ok() != Some(self.blocks.len())
                {
                    return Err(ProtocolError::Invalid(
                        "Anthropic content block index is duplicate or excessive",
                    ));
                }
                let block = value.get("content_block").ok_or(ProtocolError::Invalid(
                    "content_block_start has no content_block",
                ))?;
                let content = match required_str(block, "type")? {
                    "text" => {
                        require_only_fields(block, &["type", "text"])?;
                        self.blocks.insert(index, BlockKind::Text);
                        CanonicalContent::Text {
                            text: string(block, "text")?.to_owned(),
                            provider_options: ProviderOptions::new(),
                        }
                    }
                    "tool_use" => {
                        require_only_fields(block, &["type", "id", "name", "input"])?;
                        let id = required_str(block, "id")?.to_owned();
                        if !self.tool_ids.insert(id.clone()) {
                            return Err(ProtocolError::Invalid(
                                "Anthropic tool_use id is duplicate",
                            ));
                        }
                        self.blocks.insert(index, BlockKind::Tool);
                        self.tool_arguments.insert(index, String::new());
                        let input = block
                            .get("input")
                            .filter(|value| value.is_object())
                            .cloned()
                            .ok_or(ProtocolError::Invalid("tool_use input is not an object"))?;
                        if !input.as_object().is_some_and(serde_json::Map::is_empty) {
                            return Err(ProtocolError::Invalid(
                                "non-empty initial tool input cannot be streamed without loss",
                            ));
                        }
                        CanonicalContent::ToolCall(CanonicalToolCall {
                            id,
                            name: required_str(block, "name")?.to_owned(),
                            arguments: CanonicalToolArguments::Complete(input),
                            provider_options: ProviderOptions::new(),
                        })
                    }
                    _ => {
                        return Err(ProtocolError::Invalid(
                            "unsupported Anthropic content block",
                        ));
                    }
                };
                Ok(vec![CanonicalStreamEvent::ContentBlockStarted {
                    index,
                    content,
                }])
            }
            "content_block_delta" => {
                self.require_started()?;
                if self.message_delta {
                    return Err(ProtocolError::Invalid(
                        "Anthropic content followed message_delta",
                    ));
                }
                let index = required_index(&value)?;
                if self.completed.contains(&index) {
                    return Err(ProtocolError::Invalid("delta followed content_block_stop"));
                }
                let delta = value
                    .get("delta")
                    .ok_or(ProtocolError::Invalid("content block delta is missing"))?;
                match (self.blocks.get(&index), required_str(delta, "type")?) {
                    (Some(BlockKind::Text), "text_delta") => {
                        Ok(vec![CanonicalStreamEvent::TextDelta {
                            index,
                            text: string(delta, "text")?.to_owned(),
                        }])
                    }
                    (Some(BlockKind::Tool), "input_json_delta") => {
                        let fragment = string(delta, "partial_json")?.to_owned();
                        self.tool_argument_bytes =
                            self.tool_argument_bytes.saturating_add(fragment.len());
                        if self.tool_argument_bytes > MAX_TOOL_ARGUMENT_BYTES {
                            return Err(ProtocolError::ResponseTooLarge);
                        }
                        self.tool_arguments
                            .get_mut(&index)
                            .ok_or(ProtocolError::Invalid("tool argument state is missing"))?
                            .push_str(&fragment);
                        Ok(vec![CanonicalStreamEvent::ToolArgumentsDelta {
                            index,
                            delta: fragment,
                        }])
                    }
                    _ => Err(ProtocolError::Invalid(
                        "Anthropic delta does not match its content block",
                    )),
                }
            }
            "content_block_stop" => {
                self.require_started()?;
                if self.message_delta {
                    return Err(ProtocolError::Invalid(
                        "Anthropic content followed message_delta",
                    ));
                }
                let index = required_index(&value)?;
                let Some(block) = self.blocks.get(&index) else {
                    return Err(ProtocolError::Invalid(
                        "content_block_stop preceded content_block_start",
                    ));
                };
                if !self.completed.insert(index) {
                    return Err(ProtocolError::Invalid("duplicate content_block_stop"));
                }
                let mut events = Vec::new();
                if *block == BlockKind::Tool {
                    validate_tool_arguments(self.tool_arguments.get(&index))?;
                    events.push(CanonicalStreamEvent::ToolCallCompleted { index });
                }
                events.push(CanonicalStreamEvent::ContentBlockCompleted { index });
                Ok(events)
            }
            "message_delta" => {
                self.require_started()?;
                if self.message_delta || self.completed.len() != self.blocks.len() {
                    return Err(ProtocolError::Invalid(
                        "Anthropic message_delta is duplicate or precedes block completion",
                    ));
                }
                self.message_delta = true;
                let reason = value
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .ok_or(ProtocolError::Invalid("message_delta has no stop_reason"))?;
                validate_anthropic_finish_reason(reason)?;
                self.finish_reason = Some(reason.to_owned());
                merge_anthropic_usage(&mut self.usage, value.get("usage"))?;
                Ok(vec![
                    CanonicalStreamEvent::OutputCompleted {
                        index: 0,
                        finish_reason: reason.to_owned(),
                    },
                    CanonicalStreamEvent::Usage(self.usage.clone()),
                    CanonicalStreamEvent::MessageCompleted,
                ])
            }
            "message_stop" => {
                self.require_started()?;
                if !self.message_delta {
                    return Err(ProtocolError::Invalid(
                        "Anthropic message_stop preceded message_delta",
                    ));
                }
                self.terminal = true;
                if self.finish_reason.as_deref() == Some("max_tokens") {
                    Ok(vec![CanonicalStreamEvent::Error(CanonicalError::provider(
                        "provider_response_incomplete",
                    ))])
                } else {
                    Ok(vec![CanonicalStreamEvent::ResponseCompleted])
                }
            }
            "error" => {
                self.terminal = true;
                Ok(vec![CanonicalStreamEvent::Error(CanonicalError::provider(
                    "provider_stream_error",
                ))])
            }
            _ => Err(ProtocolError::Invalid(
                "unknown Anthropic semantic stream event",
            )),
        }
    }
}

impl AnthropicObserver {
    fn require_started(&self) -> Result<(), ProtocolError> {
        if self.started {
            Ok(())
        } else {
            Err(ProtocolError::Invalid(
                "Anthropic stream event preceded message_start",
            ))
        }
    }
}

struct ResponsesBlock {
    kind: BlockKind,
    item_id: String,
    call_id: Option<String>,
    name: Option<String>,
    text: String,
    arguments: String,
    part_started: bool,
    value_done: bool,
    content_done: bool,
    item_done: bool,
}

#[derive(Default)]
pub struct OpenAiResponsesObserver {
    started: bool,
    terminal: bool,
    response_id: Option<String>,
    next_sequence: u64,
    blocks: HashMap<u32, ResponsesBlock>,
    usage: CanonicalUsage,
    tool_argument_bytes: usize,
    item_ids: HashSet<String>,
    call_ids: HashSet<String>,
}

impl StreamObserver for OpenAiResponsesObserver {
    fn observe(&mut self, event: &[u8]) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
        let value = strict_event(event)?;
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        if self.terminal {
            return Err(ProtocolError::Invalid(
                "Responses event followed the terminal event",
            ));
        }
        let sequence =
            value
                .get("sequence_number")
                .and_then(Value::as_u64)
                .ok_or(ProtocolError::Invalid(
                    "Responses sequence_number is missing",
                ))?;
        if sequence != self.next_sequence {
            return Err(ProtocolError::Invalid(
                "Responses sequence_number is not contiguous",
            ));
        }
        self.next_sequence = self.next_sequence.saturating_add(1);
        match required_str(&value, "type")? {
            "response.created" => {
                if self.started {
                    return Err(ProtocolError::Invalid("duplicate response.created"));
                }
                let response = value
                    .get("response")
                    .ok_or(ProtocolError::Invalid("response.created has no response"))?;
                if response.get("object").and_then(Value::as_str) != Some("response")
                    || response.get("status").and_then(Value::as_str) != Some("in_progress")
                {
                    return Err(ProtocolError::Invalid(
                        "Responses created envelope is invalid",
                    ));
                }
                let id = required_str(response, "id")?.to_owned();
                self.response_id = Some(id.clone());
                self.started = true;
                Ok(vec![CanonicalStreamEvent::ResponseStarted(id)])
            }
            "response.in_progress" => {
                self.require_started()?;
                self.require_response_id(&value)?;
                Ok(Vec::new())
            }
            "response.output_item.added" => {
                self.require_started()?;
                let index = required_output_index(&value)?;
                if self.blocks.len() >= MAX_BLOCKS
                    || self.blocks.contains_key(&index)
                    || usize::try_from(index).ok() != Some(self.blocks.len())
                {
                    return Err(ProtocolError::Invalid(
                        "Responses output index is duplicate or excessive",
                    ));
                }
                let item = value
                    .get("item")
                    .ok_or(ProtocolError::Invalid("output_item.added has no item"))?;
                let item_id = required_str(item, "id")?.to_owned();
                if !self.item_ids.insert(item_id.clone()) {
                    return Err(ProtocolError::Invalid("Responses item id is duplicate"));
                }
                if item.get("status").and_then(Value::as_str) != Some("in_progress") {
                    return Err(ProtocolError::Invalid(
                        "Responses added item status is invalid",
                    ));
                }
                let (block, event) = match required_str(item, "type")? {
                    "message" => {
                        if item.get("role").and_then(Value::as_str) != Some("assistant") {
                            return Err(ProtocolError::Invalid(
                                "Responses message item is not assistant",
                            ));
                        }
                        (
                            ResponsesBlock {
                                kind: BlockKind::Text,
                                item_id,
                                call_id: None,
                                name: None,
                                text: String::new(),
                                arguments: String::new(),
                                part_started: false,
                                value_done: false,
                                content_done: false,
                                item_done: false,
                            },
                            CanonicalStreamEvent::MessageStarted {
                                role: CanonicalRole::Assistant,
                            },
                        )
                    }
                    "function_call" => {
                        let call_id = required_str(item, "call_id")?.to_owned();
                        if !self.call_ids.insert(call_id.clone()) {
                            return Err(ProtocolError::Invalid(
                                "Responses function call id is duplicate",
                            ));
                        }
                        let name = required_str(item, "name")?.to_owned();
                        (
                            ResponsesBlock {
                                kind: BlockKind::Tool,
                                item_id,
                                call_id: Some(call_id.clone()),
                                name: Some(name.clone()),
                                text: String::new(),
                                arguments: String::new(),
                                part_started: true,
                                value_done: false,
                                content_done: true,
                                item_done: false,
                            },
                            CanonicalStreamEvent::ToolCallStarted {
                                index,
                                call: CanonicalToolCall {
                                    id: call_id,
                                    name,
                                    arguments: CanonicalToolArguments::Complete(json!({})),
                                    provider_options: ProviderOptions::new(),
                                },
                            },
                        )
                    }
                    _ => return Err(ProtocolError::Invalid("unsupported Responses output item")),
                };
                self.blocks.insert(index, block);
                Ok(vec![event])
            }
            "response.content_part.added" => {
                self.require_started()?;
                let index = required_output_index(&value)?;
                let part = value
                    .get("part")
                    .ok_or(ProtocolError::Invalid("content_part.added has no part"))?;
                self.require_content_identity(&value, index)?;
                if required_content_index(&value)? != 0
                    || required_str(part, "type")? != "output_text"
                {
                    return Err(ProtocolError::Invalid(
                        "Responses content part does not match a text item",
                    ));
                }
                let block = self
                    .blocks
                    .get_mut(&index)
                    .ok_or(ProtocolError::Invalid("Responses content part has no item"))?;
                if block.kind != BlockKind::Text || block.part_started || block.item_done {
                    return Err(ProtocolError::Invalid(
                        "Responses content part is duplicate or out of order",
                    ));
                }
                let initial = string(part, "text")?.to_owned();
                block.text.push_str(&initial);
                block.part_started = true;
                Ok(vec![CanonicalStreamEvent::ContentBlockStarted {
                    index,
                    content: CanonicalContent::Text {
                        text: initial,
                        provider_options: ProviderOptions::new(),
                    },
                }])
            }
            "response.output_text.delta" => {
                let index = required_output_index(&value)?;
                self.require_content_identity(&value, index)?;
                if required_content_index(&value)? != 0 {
                    return Err(ProtocolError::Invalid(
                        "Responses text delta has unsupported content index",
                    ));
                }
                let block = self
                    .blocks
                    .get_mut(&index)
                    .ok_or(ProtocolError::Invalid("Responses text delta has no item"))?;
                if block.kind != BlockKind::Text || !block.part_started || block.value_done {
                    return Err(ProtocolError::Invalid(
                        "Responses text delta has no open text item",
                    ));
                }
                let text = string(&value, "delta")?.to_owned();
                block.text.push_str(&text);
                Ok(vec![CanonicalStreamEvent::TextDelta { index, text }])
            }
            "response.function_call_arguments.delta" => {
                let index = required_output_index(&value)?;
                self.require_item_identity(&value, index)?;
                let block = self
                    .blocks
                    .get_mut(&index)
                    .ok_or(ProtocolError::Invalid("Responses tool delta has no item"))?;
                if block.kind != BlockKind::Tool || block.value_done || block.item_done {
                    return Err(ProtocolError::Invalid(
                        "Responses tool delta has no open tool item",
                    ));
                }
                let delta = string(&value, "delta")?.to_owned();
                self.tool_argument_bytes = self.tool_argument_bytes.saturating_add(delta.len());
                if self.tool_argument_bytes > MAX_TOOL_ARGUMENT_BYTES {
                    return Err(ProtocolError::ResponseTooLarge);
                }
                block.arguments.push_str(&delta);
                Ok(vec![CanonicalStreamEvent::ToolArgumentsDelta {
                    index,
                    delta,
                }])
            }
            "response.output_text.done" => {
                let index = required_output_index(&value)?;
                self.require_content_identity(&value, index)?;
                if required_content_index(&value)? != 0 {
                    return Err(ProtocolError::Invalid(
                        "Responses text done has unsupported content index",
                    ));
                }
                let block = self
                    .blocks
                    .get_mut(&index)
                    .ok_or(ProtocolError::Invalid("Responses text done has no item"))?;
                if block.kind != BlockKind::Text
                    || !block.part_started
                    || block.value_done
                    || string(&value, "text")? != block.text
                {
                    return Err(ProtocolError::Invalid(
                        "Responses text done does not match accumulated text",
                    ));
                }
                block.value_done = true;
                Ok(Vec::new())
            }
            "response.function_call_arguments.done" => {
                let index = required_output_index(&value)?;
                self.require_item_identity(&value, index)?;
                let block = self
                    .blocks
                    .get_mut(&index)
                    .ok_or(ProtocolError::Invalid("Responses tool done has no item"))?;
                if block.kind != BlockKind::Tool
                    || block.value_done
                    || string(&value, "arguments")? != block.arguments
                {
                    return Err(ProtocolError::Invalid(
                        "Responses tool done does not match accumulated arguments",
                    ));
                }
                validate_tool_arguments(Some(&block.arguments))?;
                block.value_done = true;
                Ok(Vec::new())
            }
            "response.content_part.done" => {
                let index = required_output_index(&value)?;
                self.require_content_identity(&value, index)?;
                if required_content_index(&value)? != 0 {
                    return Err(ProtocolError::Invalid(
                        "Responses content done has unsupported content index",
                    ));
                }
                let part = value
                    .get("part")
                    .ok_or(ProtocolError::Invalid("content_part.done has no part"))?;
                let block = self
                    .blocks
                    .get_mut(&index)
                    .ok_or(ProtocolError::Invalid("Responses content done has no item"))?;
                if block.kind != BlockKind::Text
                    || !block.value_done
                    || block.content_done
                    || required_str(part, "type")? != "output_text"
                    || string(part, "text")? != block.text
                {
                    return Err(ProtocolError::Invalid(
                        "Responses content part ended without one open text item",
                    ));
                }
                block.content_done = true;
                Ok(vec![CanonicalStreamEvent::ContentBlockCompleted { index }])
            }
            "response.output_item.done" => {
                let index = required_output_index(&value)?;
                let item = value
                    .get("item")
                    .ok_or(ProtocolError::Invalid("output_item.done has no item"))?;
                let block = self.blocks.get_mut(&index).ok_or(ProtocolError::Invalid(
                    "Responses output item done has no item",
                ))?;
                if required_str(item, "id")? != block.item_id
                    || item.get("status").and_then(Value::as_str) != Some("completed")
                    || block.item_done
                {
                    return Err(ProtocolError::Invalid(
                        "Responses output item done has mismatched identity",
                    ));
                }
                let events = match block.kind {
                    BlockKind::Text if block.content_done => {
                        if required_str(item, "type")? != "message"
                            || item.get("role").and_then(Value::as_str) != Some("assistant")
                        {
                            return Err(ProtocolError::Invalid(
                                "Responses output item kind changed",
                            ));
                        }
                        vec![CanonicalStreamEvent::MessageCompleted]
                    }
                    BlockKind::Tool if block.value_done => {
                        if required_str(item, "type")? != "function_call"
                            || required_str(item, "call_id")?
                                != block.call_id.as_deref().unwrap_or("")
                            || required_str(item, "name")? != block.name.as_deref().unwrap_or("")
                            || string(item, "arguments")? != block.arguments
                        {
                            return Err(ProtocolError::Invalid(
                                "Responses completed tool item changed",
                            ));
                        }
                        vec![
                            CanonicalStreamEvent::ToolCallCompleted { index },
                            CanonicalStreamEvent::ContentBlockCompleted { index },
                        ]
                    }
                    _ => {
                        return Err(ProtocolError::Invalid(
                            "Responses output item ended before its content",
                        ));
                    }
                };
                block.item_done = true;
                Ok(events)
            }
            "response.completed" => {
                self.require_started()?;
                if self.blocks.values().any(|block| !block.item_done) {
                    return Err(ProtocolError::Invalid(
                        "Responses completed before every output item",
                    ));
                }
                let response = value
                    .get("response")
                    .ok_or(ProtocolError::Invalid("response.completed has no response"))?;
                self.require_response_value_id(response)?;
                if response.get("status").and_then(Value::as_str) != Some("completed") {
                    return Err(ProtocolError::Invalid(
                        "Responses completed event has the wrong status",
                    ));
                }
                self.validate_terminal_output(response)?;
                self.usage = openai_usage(response.get("usage"))?;
                self.terminal = true;
                let finish_reason = if self
                    .blocks
                    .values()
                    .any(|block| block.kind == BlockKind::Tool)
                {
                    "tool_use"
                } else {
                    "stop"
                };
                Ok(vec![
                    CanonicalStreamEvent::OutputCompleted {
                        index: 0,
                        finish_reason: finish_reason.to_owned(),
                    },
                    CanonicalStreamEvent::Usage(self.usage.clone()),
                    CanonicalStreamEvent::ResponseCompleted,
                ])
            }
            "response.failed" | "response.incomplete" => {
                self.require_started()?;
                let response = value
                    .get("response")
                    .ok_or(ProtocolError::Invalid("terminal response is missing"))?;
                self.require_response_value_id(response)?;
                let kind = required_str(&value, "type")?;
                let expected_status = if kind == "response.incomplete" {
                    "incomplete"
                } else {
                    "failed"
                };
                if response.get("status").and_then(Value::as_str) != Some(expected_status) {
                    return Err(ProtocolError::Invalid(
                        "Responses terminal event has the wrong status",
                    ));
                }
                let mut events = Vec::new();
                if response.get("usage").is_some_and(|usage| !usage.is_null()) {
                    self.usage = openai_usage(response.get("usage"))?;
                    events.push(CanonicalStreamEvent::Usage(self.usage.clone()));
                }
                self.terminal = true;
                events.push(CanonicalStreamEvent::Error(CanonicalError::provider(
                    if kind == "response.incomplete" {
                        "provider_response_incomplete"
                    } else {
                        "provider_response_failed"
                    },
                )));
                Ok(events)
            }
            "error" => {
                self.require_started()?;
                self.terminal = true;
                Ok(vec![CanonicalStreamEvent::Error(CanonicalError::provider(
                    "provider_stream_error",
                ))])
            }
            _ => Err(ProtocolError::Invalid(
                "unknown OpenAI semantic stream event",
            )),
        }
    }
}

impl OpenAiResponsesObserver {
    fn require_started(&self) -> Result<(), ProtocolError> {
        if self.started {
            Ok(())
        } else {
            Err(ProtocolError::Invalid(
                "Responses stream event preceded response.created",
            ))
        }
    }

    fn require_response_id(&self, value: &Value) -> Result<(), ProtocolError> {
        let response = value
            .get("response")
            .ok_or(ProtocolError::Invalid("Responses event has no response"))?;
        self.require_response_value_id(response)
    }

    fn require_response_value_id(&self, response: &Value) -> Result<(), ProtocolError> {
        if Some(required_str(response, "id")?) == self.response_id.as_deref() {
            Ok(())
        } else {
            Err(ProtocolError::Invalid("Responses response id changed"))
        }
    }

    fn require_item_identity(&self, value: &Value, index: u32) -> Result<(), ProtocolError> {
        let block = self.blocks.get(&index).ok_or(ProtocolError::Invalid(
            "Responses event references an unknown item",
        ))?;
        if required_str(value, "item_id")? == block.item_id {
            Ok(())
        } else {
            Err(ProtocolError::Invalid("Responses item id changed"))
        }
    }

    fn require_content_identity(&self, value: &Value, index: u32) -> Result<(), ProtocolError> {
        self.require_item_identity(value, index)
    }

    fn validate_terminal_output(&self, response: &Value) -> Result<(), ProtocolError> {
        let output =
            response
                .get("output")
                .and_then(Value::as_array)
                .ok_or(ProtocolError::Invalid(
                    "Responses terminal output is missing",
                ))?;
        if output.len() != self.blocks.len() {
            return Err(ProtocolError::Invalid(
                "Responses terminal output count changed",
            ));
        }
        for (index, item) in output.iter().enumerate() {
            let block = self
                .blocks
                .get(&u32::try_from(index).map_err(|_| {
                    ProtocolError::Invalid("Responses output index exceeds canonical range")
                })?)
                .ok_or(ProtocolError::Invalid(
                    "Responses terminal output index changed",
                ))?;
            if required_str(item, "id")? != block.item_id {
                return Err(ProtocolError::Invalid(
                    "Responses terminal output identity changed",
                ));
            }
            match block.kind {
                BlockKind::Text => {
                    let content = item
                        .get("content")
                        .and_then(Value::as_array)
                        .filter(|content| content.len() == 1)
                        .ok_or(ProtocolError::Invalid(
                            "Responses terminal text content changed",
                        ))?;
                    if required_str(item, "type")? != "message"
                        || item.get("status").and_then(Value::as_str) != Some("completed")
                        || item.get("role").and_then(Value::as_str) != Some("assistant")
                        || required_str(&content[0], "type")? != "output_text"
                        || string(&content[0], "text")? != block.text
                    {
                        return Err(ProtocolError::Invalid(
                            "Responses terminal text content changed",
                        ));
                    }
                }
                BlockKind::Tool => {
                    if required_str(item, "type")? != "function_call"
                        || item.get("status").and_then(Value::as_str) != Some("completed")
                        || required_str(item, "call_id")? != block.call_id.as_deref().unwrap_or("")
                        || required_str(item, "name")? != block.name.as_deref().unwrap_or("")
                        || string(item, "arguments")? != block.arguments
                    {
                        return Err(ProtocolError::Invalid(
                            "Responses terminal tool content changed",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

struct OpenAiBlock {
    kind: BlockKind,
    text: String,
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
}

pub struct OpenAiResponsesEncoder {
    response_id: String,
    response_template: Value,
    started: bool,
    terminal: bool,
    usage: CanonicalUsage,
    blocks: BTreeMap<u32, OpenAiBlock>,
    next_sequence: u64,
    finish_reason: String,
}

impl OpenAiResponsesEncoder {
    fn new(response_id: String, public_model: String, source_request: &CanonicalRequest) -> Self {
        let response_template = openai_response_envelope(
            source_request,
            &response_id,
            &public_model,
            chrono::Utc::now().timestamp(),
            "in_progress",
            json!([]),
            None,
        );
        Self {
            response_id,
            response_template,
            started: false,
            terminal: false,
            usage: CanonicalUsage::default(),
            blocks: BTreeMap::new(),
            next_sequence: 0,
            finish_reason: "stop".to_owned(),
        }
    }

    fn base_response(&self, status: &str, output: Value) -> Value {
        let mut response = self.response_template.clone();
        response["status"] = Value::String(status.to_owned());
        response["output"] = output;
        response["usage"] = Value::Null;
        response["incomplete_details"] = Value::Null;
        response
    }

    fn event(&mut self, event: &str, mut value: Value) -> Bytes {
        value["sequence_number"] = Value::from(self.next_sequence);
        self.next_sequence = self.next_sequence.saturating_add(1);
        sse(event, value)
    }
}

impl StreamEncoder for OpenAiResponsesEncoder {
    fn encode(&mut self, events: &[CanonicalStreamEvent]) -> Result<Vec<Bytes>, ProtocolError> {
        let mut output = Vec::new();
        for event in events {
            match event {
                CanonicalStreamEvent::ResponseStarted(_) => {
                    if self.started {
                        return Err(ProtocolError::Invalid("duplicate canonical response start"));
                    }
                    self.started = true;
                    let response = self.base_response("in_progress", json!([]));
                    output.push(self.event(
                        "response.created",
                        json!({"type": "response.created", "response": response}),
                    ));
                }
                CanonicalStreamEvent::ContentBlockStarted { index, content } => {
                    let (item, block) = match content {
                        CanonicalContent::Text { text, .. } => (
                            json!({"type": "message", "id": format!("msg_{}_{}", self.response_id, index), "status": "in_progress", "role": "assistant", "content": []}),
                            OpenAiBlock { kind: BlockKind::Text, text: text.clone(), call_id: None, name: None, arguments: String::new() },
                        ),
                        CanonicalContent::ToolCall(call) => (
                            json!({"type": "function_call", "id": format!("fc_{}_{}", self.response_id, index), "call_id": call.id, "name": call.name, "arguments": "", "status": "in_progress"}),
                            OpenAiBlock { kind: BlockKind::Tool, text: String::new(), call_id: Some(call.id.clone()), name: Some(call.name.clone()), arguments: String::new() },
                        ),
                        _ => return Err(ProtocolError::Invalid("unsupported canonical block")),
                    };
                    if self.blocks.insert(*index, block).is_some() {
                        return Err(ProtocolError::Invalid("duplicate canonical block index"));
                    }
                    output.push(self.event("response.output_item.added", json!({"type": "response.output_item.added", "output_index": index, "item": item})));
                    if matches!(content, CanonicalContent::Text { .. }) {
                        output.push(self.event("response.content_part.added", json!({"type": "response.content_part.added", "item_id": format!("msg_{}_{}", self.response_id, index), "output_index": index, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}})));
                    }
                }
                CanonicalStreamEvent::ToolCallStarted { index, call } => {
                    let block = OpenAiBlock { kind: BlockKind::Tool, text: String::new(), call_id: Some(call.id.clone()), name: Some(call.name.clone()), arguments: String::new() };
                    if self.blocks.insert(*index, block).is_some() {
                        return Err(ProtocolError::Invalid("duplicate canonical block index"));
                    }
                    output.push(self.event("response.output_item.added", json!({"type": "response.output_item.added", "output_index": index, "item": {"type": "function_call", "id": format!("fc_{}_{}", self.response_id, index), "call_id": call.id, "name": call.name, "arguments": "", "status": "in_progress"}})));
                }
                CanonicalStreamEvent::TextDelta { index, text } => {
                    let block = self.blocks.get_mut(index).ok_or(ProtocolError::Invalid("text delta has no block"))?;
                    if block.kind != BlockKind::Text {
                        return Err(ProtocolError::Invalid("text delta targeted a tool block"));
                    }
                    block.text.push_str(text);
                    output.push(self.event("response.output_text.delta", json!({"type": "response.output_text.delta", "item_id": format!("msg_{}_{}", self.response_id, index), "output_index": index, "content_index": 0, "delta": text})));
                }
                CanonicalStreamEvent::ToolArgumentsDelta { index, delta } => {
                    let block = self.blocks.get_mut(index).ok_or(ProtocolError::Invalid("tool delta has no block"))?;
                    if block.kind != BlockKind::Tool {
                        return Err(ProtocolError::Invalid("tool delta targeted a text block"));
                    }
                    block.arguments.push_str(delta);
                    output.push(self.event("response.function_call_arguments.delta", json!({"type": "response.function_call_arguments.delta", "output_index": index, "item_id": format!("fc_{}_{}", self.response_id, index), "delta": delta})));
                }
                CanonicalStreamEvent::ContentBlockCompleted { index } => {
                    let block = self.blocks.get_mut(index).ok_or(ProtocolError::Invalid("completed block does not exist"))?;
                    if block.kind == BlockKind::Tool && block.arguments.is_empty() {
                        block.arguments.push_str("{}");
                    }
                    let kind = block.kind;
                    let text = block.text.clone();
                    let arguments = block.arguments.clone();
                    let call_id = block.call_id.clone();
                    let name = block.name.clone();
                    match kind {
                        BlockKind::Text => {
                            let item_id = format!("msg_{}_{}", self.response_id, index);
                            output.push(self.event("response.output_text.done", json!({"type": "response.output_text.done", "item_id": item_id, "output_index": index, "content_index": 0, "text": text})));
                            output.push(self.event("response.content_part.done", json!({"type": "response.content_part.done", "item_id": item_id, "output_index": index, "content_index": 0, "part": {"type": "output_text", "text": text, "annotations": []}})));
                            output.push(self.event("response.output_item.done", json!({"type": "response.output_item.done", "output_index": index, "item": {"type": "message", "id": item_id, "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": text, "annotations": []}]}})));
                        }
                        BlockKind::Tool => {
                            let item_id = format!("fc_{}_{}", self.response_id, index);
                            output.push(self.event("response.function_call_arguments.done", json!({"type": "response.function_call_arguments.done", "output_index": index, "item_id": item_id, "arguments": arguments})));
                            output.push(self.event("response.output_item.done", json!({"type": "response.output_item.done", "output_index": index, "item": {"type": "function_call", "id": item_id, "call_id": call_id, "name": name, "arguments": arguments, "status": "completed"}})));
                        }
                    }
                }
                CanonicalStreamEvent::Usage(usage) => self.usage = usage.clone(),
                CanonicalStreamEvent::OutputCompleted { finish_reason, .. } => {
                    self.finish_reason = finish_reason.clone();
                }
                CanonicalStreamEvent::ResponseCompleted => {
                    if !self.started || self.terminal {
                        return Err(ProtocolError::Invalid("invalid canonical terminal event"));
                    }
                    self.terminal = true;
                    let items = self.blocks.iter().map(|(index, block)| match block.kind {
                        BlockKind::Text => json!({"type": "message", "id": format!("msg_{}_{}", self.response_id, index), "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": block.text, "annotations": []}]}),
                        BlockKind::Tool => json!({"type": "function_call", "id": format!("fc_{}_{}", self.response_id, index), "call_id": block.call_id, "name": block.name, "arguments": block.arguments, "status": "completed"}),
                    }).collect::<Vec<_>>();
                    let incomplete = matches!(self.finish_reason.as_str(), "max_tokens" | "max_output_tokens" | "length");
                    let status = if incomplete { "incomplete" } else { "completed" };
                    let mut response = self.base_response(status, Value::Array(items));
                    if incomplete {
                        response["incomplete_details"] = json!({"reason": "max_output_tokens"});
                    }
                    let input_tokens = self.usage.input_tokens
                        .saturating_add(self.usage.cached_input_tokens)
                        .saturating_add(self.usage.cache_write_input_tokens);
                    let output_tokens = self.usage.output_tokens
                        .saturating_add(self.usage.reasoning_tokens);
                    response["usage"] = json!({
                        "input_tokens": input_tokens,
                        "input_tokens_details": {"cached_tokens": self.usage.cached_input_tokens},
                        "output_tokens": output_tokens,
                        "output_tokens_details": {"reasoning_tokens": self.usage.reasoning_tokens},
                        "total_tokens": input_tokens.saturating_add(output_tokens)
                    });
                    let event = if incomplete { "response.incomplete" } else { "response.completed" };
                    output.push(self.event(event, json!({"type": event, "response": response})));
                }
                CanonicalStreamEvent::Error(error)
                    if error.error_type == "provider_response_incomplete" =>
                {
                    let items = self.blocks.iter().map(|(index, block)| match block.kind {
                        BlockKind::Text => json!({"type": "message", "id": format!("msg_{}_{}", self.response_id, index), "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": block.text, "annotations": []}]}),
                        BlockKind::Tool => json!({"type": "function_call", "id": format!("fc_{}_{}", self.response_id, index), "call_id": block.call_id, "name": block.name, "arguments": block.arguments, "status": "completed"}),
                    }).collect::<Vec<_>>();
                    let mut response = self.base_response("incomplete", Value::Array(items));
                    response["incomplete_details"] = json!({"reason": "max_output_tokens"});
                    let input_tokens = self.usage.input_tokens
                        .saturating_add(self.usage.cached_input_tokens)
                        .saturating_add(self.usage.cache_write_input_tokens);
                    let output_tokens = self.usage.output_tokens
                        .saturating_add(self.usage.reasoning_tokens);
                    response["usage"] = json!({
                        "input_tokens": input_tokens,
                        "input_tokens_details": {"cached_tokens": self.usage.cached_input_tokens},
                        "output_tokens": output_tokens,
                        "output_tokens_details": {"reasoning_tokens": self.usage.reasoning_tokens},
                        "total_tokens": input_tokens.saturating_add(output_tokens)
                    });
                    output.push(self.event("response.incomplete", json!({"type": "response.incomplete", "response": response})));
                }
                CanonicalStreamEvent::Error(_) => output.push(self.event("error", json!({"type": "error", "error": {"type": "api_error", "message": "upstream stream failed"}}))),
                CanonicalStreamEvent::MessageStarted { .. }
                | CanonicalStreamEvent::MessageCompleted
                | CanonicalStreamEvent::ToolCallCompleted { .. }
                | CanonicalStreamEvent::CompletionHint => {}
                CanonicalStreamEvent::ReasoningDelta { .. } => return Err(ProtocolError::Invalid("reasoning cannot be transcoded")),
            }
        }
        Ok(output)
    }
}

fn strict_event(event: &[u8]) -> Result<Option<Value>, ProtocolError> {
    let data = event_data(event);
    if !data.has_data {
        return if data.has_non_comment_field {
            Err(ProtocolError::Invalid("SSE event is missing data:"))
        } else {
            Ok(None)
        };
    }
    serde_json::from_slice(&data.bytes)
        .map(Some)
        .map_err(|_| ProtocolError::Invalid("SSE data is not valid JSON"))
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, ProtocolError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(ProtocolError::Invalid("required SSE string is missing"))
}

fn require_only_fields(value: &Value, allowed: &[&str]) -> Result<(), ProtocolError> {
    value
        .as_object()
        .filter(|object| object.keys().all(|key| allowed.contains(&key.as_str())))
        .map(|_| ())
        .ok_or(ProtocolError::Invalid(
            "translated content contains unsupported fields",
        ))
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ProtocolError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(ProtocolError::Invalid("required SSE string is missing"))
}

fn required_index(value: &Value) -> Result<u32, ProtocolError> {
    value
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(ProtocolError::Invalid("SSE index is missing or too large"))
}

fn required_output_index(value: &Value) -> Result<u32, ProtocolError> {
    value
        .get("output_index")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(ProtocolError::Invalid(
            "Responses output_index is missing or too large",
        ))
}

fn required_content_index(value: &Value) -> Result<u32, ProtocolError> {
    value
        .get("content_index")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(ProtocolError::Invalid(
            "Responses content_index is missing or too large",
        ))
}

fn anthropic_initial_usage(value: Option<&Value>) -> Result<CanonicalUsage, ProtocolError> {
    let object = value
        .and_then(Value::as_object)
        .ok_or(ProtocolError::Invalid("Anthropic usage is missing"))?;
    Ok(CanonicalUsage {
        input_tokens: usage_number(object, "input_tokens")?,
        output_tokens: optional_usage_number(object, "output_tokens")?,
        cached_input_tokens: optional_usage_number(object, "cache_read_input_tokens")?,
        cache_write_input_tokens: optional_usage_number(object, "cache_creation_input_tokens")?,
        reasoning_tokens: 0,
        provider_options: ProviderOptions::new(),
    })
}

fn merge_anthropic_usage(
    usage: &mut CanonicalUsage,
    value: Option<&Value>,
) -> Result<(), ProtocolError> {
    let object = value
        .and_then(Value::as_object)
        .ok_or(ProtocolError::Invalid("Anthropic usage is missing"))?;
    if object.is_empty() {
        return Err(ProtocolError::Invalid("Anthropic usage is empty"));
    }
    if object.contains_key("input_tokens") {
        usage.input_tokens = usage_number(object, "input_tokens")?;
    }
    if object.contains_key("output_tokens") {
        usage.output_tokens = usage_number(object, "output_tokens")?;
    }
    if object.contains_key("cache_read_input_tokens") {
        usage.cached_input_tokens = usage_number(object, "cache_read_input_tokens")?;
    }
    if object.contains_key("cache_creation_input_tokens") {
        usage.cache_write_input_tokens = usage_number(object, "cache_creation_input_tokens")?;
    }
    Ok(())
}

fn openai_usage(value: Option<&Value>) -> Result<CanonicalUsage, ProtocolError> {
    let object = value
        .and_then(Value::as_object)
        .ok_or(ProtocolError::Invalid("OpenAI usage is missing"))?;
    Ok(CanonicalUsage {
        input_tokens: usage_number(object, "input_tokens")?,
        output_tokens: usage_number(object, "output_tokens")?,
        cached_input_tokens: object
            .get("input_tokens_details")
            .and_then(Value::as_object)
            .map_or(Ok(0), |details| {
                optional_usage_number(details, "cached_tokens")
            })?,
        cache_write_input_tokens: object
            .get("input_tokens_details")
            .and_then(Value::as_object)
            .map_or(Ok(0), |details| {
                optional_usage_number(details, "cache_write_tokens")
            })?,
        reasoning_tokens: object
            .get("output_tokens_details")
            .and_then(Value::as_object)
            .map_or(Ok(0), |details| {
                optional_usage_number(details, "reasoning_tokens")
            })?,
        provider_options: ProviderOptions::new(),
    })
}

fn usage_number(object: &serde_json::Map<String, Value>, key: &str) -> Result<u64, ProtocolError> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(ProtocolError::Invalid("required usage counter is missing"))
}

fn optional_usage_number(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<u64, ProtocolError> {
    match object.get(key) {
        None => Ok(0),
        Some(value) => value
            .as_u64()
            .ok_or(ProtocolError::Invalid("usage counter is not an integer")),
    }
}

fn sse(event: &str, value: Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {value}\n\n"))
}

fn validate_tool_arguments(arguments: Option<&String>) -> Result<(), ProtocolError> {
    let arguments = arguments.map(String::as_str).unwrap_or("{}");
    if arguments.is_empty() {
        return Ok(());
    }
    serde_json::from_str::<Value>(arguments)
        .ok()
        .filter(Value::is_object)
        .map(|_| ())
        .ok_or(ProtocolError::Invalid(
            "streamed tool arguments are not one JSON object",
        ))
}

fn validate_anthropic_finish_reason(reason: &str) -> Result<(), ProtocolError> {
    match reason {
        "end_turn" | "max_tokens" | "tool_use" => Ok(()),
        _ => Err(ProtocolError::Invalid(
            "Anthropic stop reason cannot be translated without loss",
        )),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Bytes;
    use uuid::Uuid;

    use super::{CrossStreamEncoder, CrossStreamObserver};
    use crate::{
        adapters::protocol,
        canonical::{CanonicalStreamEvent, PrincipalRef, RequestEnrichment},
        model_registry::GatewayOperation,
        services::streaming::{StreamEncoder, StreamObserver},
    };

    #[test]
    fn anthropic_text_sequence_transcodes_to_responses_events() {
        let mut observer = CrossStreamObserver::new(GatewayOperation::AnthropicMessages).unwrap();
        let mut encoder = CrossStreamEncoder::new(
            GatewayOperation::OpenAiResponses,
            "resp_apg_test".to_owned(),
            "public".to_owned(),
            &source_request(),
        )
        .unwrap();
        let frames = [
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_target\",\"type\":\"message\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":2,\"output_tokens\":0}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ];
        let mut wire = Vec::new();
        for frame in frames {
            let events = observer.observe(frame.as_bytes()).unwrap();
            wire.extend(encoder.encode(&events).unwrap());
        }
        let wire = String::from_utf8(wire.concat()).unwrap();
        assert!(wire.contains("response.output_text.delta"));
        assert!(wire.contains("resp_apg_test"));
        assert!(!wire.contains("msg_target"));
        assert!(wire.contains("response.completed"));
        let events = wire
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(events.iter().enumerate().all(|(index, event)| {
            event["sequence_number"].as_u64() == u64::try_from(index).ok()
        }));
        let delta = events
            .iter()
            .find(|event| event["type"] == "response.output_text.delta")
            .unwrap();
        assert_eq!(delta["content_index"], 0);
        assert!(delta["item_id"].as_str().unwrap().starts_with("msg_"));
        let created = &events[0]["response"];
        let terminal = &events.last().unwrap()["response"];
        assert_eq!(created["created_at"], terminal["created_at"]);
        assert_eq!(created["max_output_tokens"], 8);
        assert_eq!(created["tool_choice"], "auto");
    }

    #[test]
    fn anthropic_token_limit_stream_is_incomplete_not_completed() {
        let mut observer = CrossStreamObserver::new(GatewayOperation::AnthropicMessages).unwrap();
        let mut encoder = CrossStreamEncoder::new(
            GatewayOperation::OpenAiResponses,
            "resp_apg_test".to_owned(),
            "public".to_owned(),
            &source_request(),
        )
        .unwrap();
        let frames = [
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_target\",\"type\":\"message\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":2,\"output_tokens\":0}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":8}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ];
        let mut canonical_terminal = None;
        let mut wire = Vec::new();
        for frame in frames {
            let events = observer.observe(frame.as_bytes()).unwrap();
            if let Some(event) = events.last() {
                canonical_terminal = Some(event.clone());
            }
            wire.extend(encoder.encode(&events).unwrap());
        }
        assert!(matches!(
            canonical_terminal,
            Some(crate::canonical::CanonicalStreamEvent::Error(ref error))
                if error.error_type == "provider_response_incomplete"
        ));
        let wire = String::from_utf8(wire.concat()).unwrap();
        assert!(wire.contains("response.incomplete"));
        assert!(!wire.contains("event: response.completed"));
        assert!(wire.contains("partial"));
    }

    #[test]
    fn unknown_semantic_event_and_delta_before_start_are_rejected() {
        let mut anthropic = CrossStreamObserver::new(GatewayOperation::AnthropicMessages).unwrap();
        assert!(
            anthropic
                .observe(b"data: {\"type\":\"future_semantic_event\"}\n\n")
                .is_err()
        );
        let mut openai = CrossStreamObserver::new(GatewayOperation::OpenAiResponses).unwrap();
        assert!(openai
            .observe(b"data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"item_id\":\"msg_1\",\"content_index\":0,\"delta\":\"x\",\"sequence_number\":0}\n\n")
            .is_err());
    }

    #[test]
    fn responses_stream_cannot_claim_anthropic_usage_before_it_exists() {
        assert!(
            CrossStreamEncoder::new(
                GatewayOperation::AnthropicMessages,
                "msg_apg_test".to_owned(),
                "public".to_owned(),
                &source_request(),
            )
            .is_err()
        );
    }

    #[test]
    fn responses_rejects_gaps_changed_ids_and_done_value_mismatch() {
        let mut gap = CrossStreamObserver::new(GatewayOperation::OpenAiResponses).unwrap();
        assert!(
            gap.observe(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"in_progress\"},\"sequence_number\":1}\n\n")
                .is_err()
        );

        let mut changed = CrossStreamObserver::new(GatewayOperation::OpenAiResponses).unwrap();
        changed
            .observe(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"in_progress\"},\"sequence_number\":0}\n\n")
            .unwrap();
        assert!(
            changed
                .observe(b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_2\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}},\"sequence_number\":1}\n\n")
                .is_err()
        );

        let mut mismatch = CrossStreamObserver::new(GatewayOperation::OpenAiResponses).unwrap();
        mismatch
            .observe(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"in_progress\"},\"sequence_number\":0}\n\n")
            .unwrap();
        mismatch
            .observe(b"data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"status\":\"in_progress\",\"role\":\"assistant\"},\"sequence_number\":1}\n\n")
            .unwrap();
        mismatch
            .observe(b"data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"item_id\":\"msg_1\",\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"\"},\"sequence_number\":2}\n\n")
            .unwrap();
        mismatch
            .observe(b"data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"item_id\":\"msg_1\",\"content_index\":0,\"delta\":\"actual\",\"sequence_number\":3}\n\n")
            .unwrap();
        assert!(
            mismatch
                .observe(b"data: {\"type\":\"response.output_text.done\",\"output_index\":0,\"item_id\":\"msg_1\",\"content_index\":0,\"text\":\"changed\",\"sequence_number\":4}\n\n")
                .is_err()
        );
    }

    #[test]
    fn failed_responses_emits_usage_before_error() {
        let mut observer = CrossStreamObserver::new(GatewayOperation::OpenAiResponses).unwrap();
        observer
            .observe(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"in_progress\"},\"sequence_number\":0}\n\n")
            .unwrap();
        let events = observer
            .observe(b"data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_1\",\"status\":\"incomplete\",\"usage\":{\"input_tokens\":9,\"output_tokens\":4,\"input_tokens_details\":{\"cache_write_tokens\":5}}},\"sequence_number\":1}\n\n")
            .unwrap();
        let crate::canonical::CanonicalStreamEvent::Usage(usage) = &events[0] else {
            panic!("usage must precede the terminal error");
        };
        assert_eq!(usage.cache_write_input_tokens, 5);
        assert!(matches!(
            events[1],
            crate::canonical::CanonicalStreamEvent::Error(_)
        ));
    }

    #[test]
    fn streamed_tool_arguments_must_finish_as_one_json_object() {
        let mut observer = CrossStreamObserver::new(GatewayOperation::AnthropicMessages).unwrap();
        observer
            .observe(b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":1}}}\n\n")
            .unwrap();
        observer
            .observe(b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"lookup\",\"input\":{}}}\n\n")
            .unwrap();
        observer
            .observe(b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{broken\"}}\n\n")
            .unwrap();
        assert!(
            observer
                .observe(b"data: {\"type\":\"content_block_stop\",\"index\":0}\n\n")
                .is_err()
        );
    }

    #[test]
    fn zero_argument_anthropic_tool_stream_emits_a_round_trippable_object() {
        let mut observer = CrossStreamObserver::new(GatewayOperation::AnthropicMessages).unwrap();
        let mut encoder = CrossStreamEncoder::new(
            GatewayOperation::OpenAiResponses,
            "resp_apg_test".to_owned(),
            "public".to_owned(),
            &source_request(),
        )
        .unwrap();
        let frames = [
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_target\",\"type\":\"message\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":1}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"lookup\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":1}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ];
        let mut wire = Vec::new();
        for frame in frames {
            let events = observer.observe(frame.as_bytes()).unwrap();
            wire.extend(encoder.encode(&events).unwrap());
        }
        let events = String::from_utf8(wire.concat())
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let generated_call = events
            .iter()
            .find(|event| event["type"] == "response.output_item.done")
            .map(|event| event["item"].clone())
            .unwrap();
        assert_eq!(generated_call["arguments"], "{}");

        let next_request = serde_json::json!({
            "model": "public",
            "input": [
                generated_call,
                {"type": "function_call_output", "call_id": "call_1", "output": "found"}
            ],
            "max_output_tokens": 8
        });
        assert!(
            protocol::decode(
                GatewayOperation::OpenAiResponses,
                Bytes::from(serde_json::to_vec(&next_request).unwrap()),
            )
            .is_ok()
        );
    }

    #[test]
    fn anthropic_stream_rejects_gaps_duplicate_tool_ids_and_lossy_stop_reasons() {
        let mut gap = CrossStreamObserver::new(GatewayOperation::AnthropicMessages).unwrap();
        gap.observe(b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":1}}}\n\n")
            .unwrap();
        assert!(
            gap.observe(b"data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n")
                .is_err()
        );

        let mut duplicate = CrossStreamObserver::new(GatewayOperation::AnthropicMessages).unwrap();
        duplicate.observe(b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":1}}}\n\n").unwrap();
        duplicate.observe(b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"same\",\"name\":\"one\",\"input\":{}}}\n\n").unwrap();
        duplicate
            .observe(b"data: {\"type\":\"content_block_stop\",\"index\":0}\n\n")
            .unwrap();
        assert!(duplicate.observe(b"data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"same\",\"name\":\"two\",\"input\":{}}}\n\n").is_err());

        let mut stop = CrossStreamObserver::new(GatewayOperation::AnthropicMessages).unwrap();
        stop.observe(b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":1}}}\n\n").unwrap();
        assert!(stop.observe(b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"pause_turn\"},\"usage\":{\"output_tokens\":1}}\n\n").is_err());

        let mut initial_input =
            CrossStreamObserver::new(GatewayOperation::AnthropicMessages).unwrap();
        initial_input.observe(b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":1}}}\n\n").unwrap();
        assert!(initial_input.observe(b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"one\",\"input\":{\"already\":\"present\"}}}\n\n").is_err());
    }

    #[test]
    fn responses_stream_rejects_duplicate_item_and_call_ids() {
        let mut observer = CrossStreamObserver::new(GatewayOperation::OpenAiResponses).unwrap();
        observer.observe(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"in_progress\"},\"sequence_number\":0}\n\n").unwrap();
        observer.observe(b"data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"status\":\"in_progress\",\"call_id\":\"call_1\",\"name\":\"one\"},\"sequence_number\":1}\n\n").unwrap();
        assert!(observer.observe(b"data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"item_2\",\"status\":\"in_progress\",\"call_id\":\"call_1\",\"name\":\"two\"},\"sequence_number\":2}\n\n").is_err());

        let mut items = CrossStreamObserver::new(GatewayOperation::OpenAiResponses).unwrap();
        items.observe(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"in_progress\"},\"sequence_number\":0}\n\n").unwrap();
        items.observe(b"data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"same\",\"status\":\"in_progress\",\"role\":\"assistant\"},\"sequence_number\":1}\n\n").unwrap();
        assert!(items.observe(b"data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"message\",\"id\":\"same\",\"status\":\"in_progress\",\"role\":\"assistant\"},\"sequence_number\":2}\n\n").is_err());
    }

    #[test]
    fn gemini_stream_requires_terminal_usage_and_rejects_midstream_errors() {
        let mut observer =
            CrossStreamObserver::new(GatewayOperation::GeminiGenerateContent).unwrap();
        let first = observer
            .observe(b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hel\"}]}}]}\n\n")
            .unwrap();
        assert!(first.iter().any(
            |event| matches!(event, CanonicalStreamEvent::TextDelta { text, .. } if text == "hel")
        ));
        let terminal = observer
            .observe(b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"lo\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":1}}\n\n")
            .unwrap();
        assert!(matches!(
            terminal.last(),
            Some(CanonicalStreamEvent::ResponseCompleted)
        ));
        assert!(observer.observe(b"data: {\"candidates\":[]}\n\n").is_err());

        let mut missing_usage =
            CrossStreamObserver::new(GatewayOperation::GeminiGenerateContent).unwrap();
        assert!(missing_usage.observe(b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"x\"}]},\"finishReason\":\"STOP\"}]}\n\n").is_err());

        let mut failed = CrossStreamObserver::new(GatewayOperation::GeminiGenerateContent).unwrap();
        let events = failed
            .observe(b"data: {\"error\":{\"code\":503,\"message\":\"unavailable\"}}\n\n")
            .unwrap();
        assert!(matches!(
            events.as_slice(),
            [CanonicalStreamEvent::Error(_)]
        ));
        assert!(failed.observe(b"data: {\"candidates\":[]}\n\n").is_err());
    }

    fn source_request() -> crate::canonical::CanonicalRequest {
        protocol::decode(
            GatewayOperation::OpenAiResponses,
            Bytes::from_static(br#"{"model":"public","max_output_tokens":8,"input":"hello"}"#),
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
}
