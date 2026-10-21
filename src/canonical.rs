use std::{collections::BTreeMap, fmt};

use axum::body::Bytes;
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::{identity::PrincipalContext, ledger::TokenUsage, model_registry::GatewayOperation};

pub type ProviderOptions = BTreeMap<String, Value>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrincipalRef {
    pub tenant_id: Uuid,
    pub credential_id: Uuid,
    pub user_id: Option<Uuid>,
    pub device_id: Option<Uuid>,
    pub client_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
}

impl From<&PrincipalContext> for PrincipalRef {
    fn from(value: &PrincipalContext) -> Self {
        Self {
            tenant_id: value.tenant_id,
            credential_id: value.credential_id,
            user_id: value.user_id,
            device_id: value.device_id,
            client_id: value.client_id,
            agent_id: value.agent_id,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum CanonicalRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
    Other(String),
}

impl fmt::Debug for CanonicalRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::System => "System",
            Self::Developer => "Developer",
            Self::User => "User",
            Self::Assistant => "Assistant",
            Self::Tool => "Tool",
            Self::Other(_) => "Other",
        })
    }
}

impl CanonicalRole {
    pub fn from_wire(value: &str) -> Self {
        match value {
            "system" => Self::System,
            "developer" => Self::Developer,
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "tool" => Self::Tool,
            other => Self::Other(other.to_owned()),
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum CanonicalContent {
    Text {
        text: String,
        provider_options: ProviderOptions,
    },
    Reasoning {
        text: String,
        provider_options: ProviderOptions,
    },
    Image {
        source: Value,
        provider_options: ProviderOptions,
    },
    ToolCall(CanonicalToolCall),
    ToolResult {
        tool_call_id: String,
        content: Value,
        provider_options: ProviderOptions,
    },
    Opaque {
        provider: String,
        value: Value,
    },
}

impl fmt::Debug for CanonicalContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Text { .. } => "text",
            Self::Reasoning { .. } => "reasoning",
            Self::Image { .. } => "image",
            Self::ToolCall(_) => "tool_call",
            Self::ToolResult { .. } => "tool_result",
            Self::Opaque { .. } => "opaque",
        };
        formatter
            .debug_struct("CanonicalContent")
            .field("kind", &kind)
            .finish()
    }
}

#[derive(Clone, PartialEq)]
pub struct CanonicalMessage {
    pub role: CanonicalRole,
    pub name: Option<String>,
    pub content: Vec<CanonicalContent>,
    pub provider_options: ProviderOptions,
}

impl fmt::Debug for CanonicalMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalMessage")
            .field("role", &self.role)
            .field("has_name", &self.name.is_some())
            .field("content_items", &self.content.len())
            .field("provider_option_names", &self.provider_options.keys())
            .finish()
    }
}

#[derive(Clone, PartialEq)]
pub struct CanonicalToolDefinition {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub strict: Option<bool>,
    pub provider_options: ProviderOptions,
}

impl fmt::Debug for CanonicalToolDefinition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalToolDefinition")
            .field("name", &"[redacted]")
            .field("has_description", &self.description.is_some())
            .field("strict", &self.strict)
            .field("provider_option_names", &self.provider_options.keys())
            .finish()
    }
}

#[derive(Clone, PartialEq)]
pub enum CanonicalToolArguments {
    Complete(Value),
    Delta(String),
}

impl fmt::Debug for CanonicalToolArguments {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Complete(_) => "Complete([redacted])",
            Self::Delta(_) => "Delta([redacted])",
        })
    }
}

#[derive(Clone, PartialEq)]
pub struct CanonicalToolCall {
    pub id: String,
    pub name: String,
    pub arguments: CanonicalToolArguments,
    pub provider_options: ProviderOptions,
}

impl fmt::Debug for CanonicalToolCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalToolCall")
            .field("id", &"[redacted]")
            .field("name", &"[redacted]")
            .field("provider_option_names", &self.provider_options.keys())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Default, Eq, PartialEq)]
pub struct CanonicalUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub provider_options: ProviderOptions,
}

impl fmt::Debug for CanonicalUsage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalUsage")
            .field("input_tokens", &self.input_tokens)
            .field("output_tokens", &self.output_tokens)
            .field("cached_input_tokens", &self.cached_input_tokens)
            .field("cache_write_input_tokens", &self.cache_write_input_tokens)
            .field("reasoning_tokens", &self.reasoning_tokens)
            .field("provider_option_names", &self.provider_options.keys())
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum CanonicalToolChoice {
    Auto,
    Required,
    None,
    Named(String),
}

impl fmt::Debug for CanonicalToolChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "Auto",
            Self::Required => "Required",
            Self::None => "None",
            Self::Named(_) => "Named([redacted])",
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CanonicalReasoning {
    pub enabled: Option<bool>,
    pub effort: Option<String>,
    pub budget_tokens: Option<u64>,
    pub summary: Option<String>,
}

#[derive(Clone, PartialEq)]
pub struct CanonicalStructuredOutput {
    pub kind: CanonicalStructuredOutputKind,
    pub name: Option<String>,
    pub description: Option<String>,
    pub schema: Value,
    pub strict: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalStructuredOutputKind {
    JsonObject,
    JsonSchema,
}

impl fmt::Debug for CanonicalStructuredOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalStructuredOutput")
            .field("kind", &self.kind)
            .field("has_name", &self.name.is_some())
            .field("has_description", &self.description.is_some())
            .field("strict", &self.strict)
            .field("schema", &"[redacted]")
            .finish()
    }
}

impl From<TokenUsage> for CanonicalUsage {
    fn from(value: TokenUsage) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            cached_input_tokens: value.cached_input_tokens,
            cache_write_input_tokens: value.cache_write_input_tokens,
            reasoning_tokens: value.reasoning_tokens,
            provider_options: ProviderOptions::new(),
        }
    }
}

impl From<CanonicalUsage> for TokenUsage {
    fn from(value: CanonicalUsage) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            cached_input_tokens: value.cached_input_tokens,
            cache_write_input_tokens: value.cache_write_input_tokens,
            reasoning_tokens: value.reasoning_tokens,
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct CanonicalError {
    pub error_type: String,
    pub message: Option<String>,
    pub retryable: bool,
    pub status_code: Option<u16>,
    pub provider_options: ProviderOptions,
}

impl fmt::Debug for CanonicalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalError")
            .field("error_type", &self.error_type)
            .field("message", &self.message.as_ref().map(|_| "[redacted]"))
            .field("retryable", &self.retryable)
            .field("status_code", &self.status_code)
            .finish_non_exhaustive()
    }
}

impl CanonicalError {
    pub fn provider(error_type: impl Into<String>) -> Self {
        Self {
            error_type: error_type.into(),
            message: None,
            retryable: false,
            status_code: None,
            provider_options: ProviderOptions::new(),
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum CanonicalStreamEvent {
    ResponseStarted(String),
    MessageStarted {
        role: CanonicalRole,
    },
    ContentBlockStarted {
        index: u32,
        content: CanonicalContent,
    },
    TextDelta {
        index: u32,
        text: String,
    },
    ReasoningDelta {
        index: u32,
        text: String,
    },
    ToolCallStarted {
        index: u32,
        call: CanonicalToolCall,
    },
    ToolArgumentsDelta {
        index: u32,
        delta: String,
    },
    ToolCallCompleted {
        index: u32,
    },
    ContentBlockCompleted {
        index: u32,
    },
    MessageCompleted,
    OutputCompleted {
        index: u32,
        finish_reason: String,
    },
    Usage(CanonicalUsage),
    CompletionHint,
    ResponseCompleted,
    Error(CanonicalError),
}

impl fmt::Debug for CanonicalStreamEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::ResponseStarted(_) => "response_started",
            Self::MessageStarted { .. } => "message_started",
            Self::ContentBlockStarted { .. } => "content_block_started",
            Self::TextDelta { .. } => "text_delta",
            Self::ReasoningDelta { .. } => "reasoning_delta",
            Self::ToolCallStarted { .. } => "tool_call_started",
            Self::ToolArgumentsDelta { .. } => "tool_arguments_delta",
            Self::ToolCallCompleted { .. } => "tool_call_completed",
            Self::ContentBlockCompleted { .. } => "content_block_completed",
            Self::MessageCompleted => "message_completed",
            Self::OutputCompleted { .. } => "output_completed",
            Self::Usage(_) => "usage",
            Self::CompletionHint => "completion_hint",
            Self::ResponseCompleted => "response_completed",
            Self::Error(_) => "error",
        };
        formatter
            .debug_struct("CanonicalStreamEvent")
            .field("kind", &kind)
            .finish()
    }
}

#[derive(Clone, PartialEq)]
pub struct CanonicalResponse {
    pub response_id: Option<String>,
    pub model: Option<String>,
    pub messages: Vec<CanonicalMessage>,
    pub usage: CanonicalUsage,
    pub finish_reason: Option<String>,
    pub error: Option<CanonicalError>,
    pub provider_options: ProviderOptions,
    pub metadata: BTreeMap<String, Value>,
}

impl fmt::Debug for CanonicalResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalResponse")
            .field("response_id", &self.response_id)
            .field("model", &self.model)
            .field("message_count", &self.messages.len())
            .field("usage", &self.usage)
            .field("finish_reason", &self.finish_reason)
            .field("has_error", &self.error.is_some())
            .finish_non_exhaustive()
    }
}

pub struct CanonicalRequest {
    pub gateway_request_id: Uuid,
    pub trace_id: Option<String>,
    pub session_id: Option<String>,
    pub principal: PrincipalRef,
    pub requested_model: String,
    pub requested_quality: Option<String>,
    pub workload_class: String,
    pub urgency: String,
    pub budget_class: String,
    pub deadline: Option<DateTime<Utc>>,
    pub messages: Vec<CanonicalMessage>,
    pub tools: Vec<CanonicalToolDefinition>,
    pub tool_choice: Option<CanonicalToolChoice>,
    pub reasoning: Option<CanonicalReasoning>,
    pub structured_output: Option<CanonicalStructuredOutput>,
    pub stream: bool,
    pub max_output_tokens: Option<u64>,
    pub provider_options: ProviderOptions,
    pub metadata: BTreeMap<String, Value>,
    native: NativeWire,
}

impl fmt::Debug for CanonicalRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalRequest")
            .field("gateway_request_id", &self.gateway_request_id)
            .field("trace_id", &self.trace_id)
            .field(
                "session_id",
                &self.session_id.as_ref().map(|_| "[redacted]"),
            )
            .field("principal", &self.principal)
            .field("requested_model", &self.requested_model)
            .field("message_count", &self.messages.len())
            .field("tool_count", &self.tools.len())
            .field("stream", &self.stream)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("provider_option_names", &self.provider_options.keys())
            .finish_non_exhaustive()
    }
}

pub struct RequestEnrichment {
    pub gateway_request_id: Uuid,
    pub trace_id: Option<String>,
    pub session_id: Option<String>,
    pub principal: PrincipalRef,
    pub requested_model: String,
    pub requested_quality: Option<String>,
    pub workload_class: String,
    pub urgency: String,
    pub budget_class: String,
    pub deadline: Option<DateTime<Utc>>,
}

pub(crate) struct CanonicalRequestPayload {
    pub messages: Vec<CanonicalMessage>,
    pub tools: Vec<CanonicalToolDefinition>,
    pub tool_choice: Option<CanonicalToolChoice>,
    pub reasoning: Option<CanonicalReasoning>,
    pub structured_output: Option<CanonicalStructuredOutput>,
    pub stream: bool,
    pub max_output_tokens: Option<u64>,
    pub provider_options: ProviderOptions,
    pub metadata: BTreeMap<String, Value>,
}

pub(crate) struct NativeWire {
    pub operation: GatewayOperation,
    pub body: Bytes,
    pub document: Value,
}

impl CanonicalRequest {
    pub(crate) fn from_parts(
        enrichment: RequestEnrichment,
        payload: CanonicalRequestPayload,
        native: NativeWire,
    ) -> Self {
        Self {
            gateway_request_id: enrichment.gateway_request_id,
            trace_id: enrichment.trace_id,
            session_id: enrichment.session_id,
            principal: enrichment.principal,
            requested_model: enrichment.requested_model,
            requested_quality: enrichment.requested_quality,
            workload_class: enrichment.workload_class,
            urgency: enrichment.urgency,
            budget_class: enrichment.budget_class,
            deadline: enrichment.deadline,
            messages: payload.messages,
            tools: payload.tools,
            tool_choice: payload.tool_choice,
            reasoning: payload.reasoning,
            structured_output: payload.structured_output,
            stream: payload.stream,
            max_output_tokens: payload.max_output_tokens,
            provider_options: payload.provider_options,
            metadata: payload.metadata,
            native,
        }
    }

    pub(crate) fn native(&self) -> &NativeWire {
        &self.native
    }

    pub fn source_operation(&self) -> GatewayOperation {
        self.native.operation
    }

    pub(crate) fn source_document(&self) -> &Value {
        &self.native.document
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{CanonicalStructuredOutput, CanonicalToolChoice, CanonicalUsage, ProviderOptions};

    #[test]
    fn debug_redacts_provider_values_schemas_and_named_tools() {
        let usage = CanonicalUsage {
            provider_options: ProviderOptions::from([(
                "vendor".to_owned(),
                json!({"secret": "usage-sentinel"}),
            )]),
            ..CanonicalUsage::default()
        };
        let structured = CanonicalStructuredOutput {
            kind: super::CanonicalStructuredOutputKind::JsonSchema,
            name: Some("schema-name-sentinel".to_owned()),
            description: Some("schema-description-sentinel".to_owned()),
            schema: json!({"const": "schema-value-sentinel"}),
            strict: Some(true),
        };
        let rendered = format!(
            "{usage:?} {structured:?} {:?}",
            CanonicalToolChoice::Named("tool-name-sentinel".to_owned())
        );
        for sentinel in [
            "usage-sentinel",
            "schema-name-sentinel",
            "schema-description-sentinel",
            "schema-value-sentinel",
            "tool-name-sentinel",
        ] {
            assert!(!rendered.contains(sentinel), "Debug leaked {sentinel}");
        }
    }
}
