use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    canonical::{
        CanonicalContent, CanonicalRequest, CanonicalRole, CanonicalStructuredOutputKind,
        CanonicalUsage,
    },
    error::GatewayError,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    contents: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
}

#[derive(Serialize)]
struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
    parts: Vec<RequestPart>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum RequestPart {
    Text { text: String },
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_mime_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_schema: Option<Value>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    pub usage_metadata: Option<Usage>,
    pub error: Option<Value>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub content: ResponseContent,
    pub finish_reason: Option<String>,
}

#[derive(Clone, Deserialize)]
pub struct ResponseContent {
    pub parts: Vec<ResponsePart>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponsePart {
    pub text: Option<String>,
    pub function_call: Option<Value>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default)]
    pub prompt_token_count: u64,
    #[serde(default)]
    pub cached_content_token_count: u64,
    #[serde(default)]
    pub candidates_token_count: u64,
    #[serde(default)]
    pub thoughts_token_count: u64,
}

pub fn encode(request: &CanonicalRequest) -> Result<Vec<u8>, GatewayError> {
    if request.reasoning.is_some() || !request.tools.is_empty() || request.tool_choice.is_some() {
        return Err(GatewayError::UnsupportedRequest);
    }
    let mut system_parts = Vec::new();
    let mut contents = Vec::new();
    for message in &request.messages {
        let parts = message
            .content
            .iter()
            .map(|content| match content {
                CanonicalContent::Text { text, .. } => Ok(RequestPart::Text { text: text.clone() }),
                CanonicalContent::ToolCall(_) => Err(GatewayError::UnsupportedRequest),
                _ => Err(GatewayError::UnsupportedRequest),
            })
            .collect::<Result<Vec<_>, _>>()?;
        match message.role {
            CanonicalRole::System | CanonicalRole::Developer => system_parts.extend(parts),
            CanonicalRole::User => contents.push(Content {
                role: Some("user"),
                parts,
            }),
            CanonicalRole::Assistant => contents.push(Content {
                role: Some("model"),
                parts,
            }),
            CanonicalRole::Tool | CanonicalRole::Other(_) => {
                return Err(GatewayError::UnsupportedRequest);
            }
        }
    }
    let output = request.structured_output.as_ref();
    let generation = GenerationConfig {
        max_output_tokens: request.max_output_tokens,
        response_mime_type: output.map(|_| "application/json"),
        response_schema: output
            .filter(|value| value.kind == CanonicalStructuredOutputKind::JsonSchema)
            .map(|value| value.schema.clone()),
    };
    let generation_config = (generation.max_output_tokens.is_some()
        || generation.response_mime_type.is_some())
    .then_some(generation);
    serde_json::to_vec(&Request {
        contents,
        system_instruction: (!system_parts.is_empty()).then_some(Content {
            role: None,
            parts: system_parts,
        }),
        generation_config,
    })
    .map_err(GatewayError::InvalidRequest)
}

pub fn decode(value: Value) -> Result<Response, GatewayError> {
    serde_json::from_value(value).map_err(|_| GatewayError::UpstreamProtocol)
}

pub fn usage(value: &Value) -> Result<Option<CanonicalUsage>, GatewayError> {
    let response = decode(value.clone())?;
    response
        .usage_metadata
        .map(|usage| {
            Ok(CanonicalUsage {
                input_tokens: usage
                    .prompt_token_count
                    .checked_sub(usage.cached_content_token_count)
                    .ok_or(GatewayError::UpstreamProtocol)?,
                output_tokens: usage.candidates_token_count,
                cached_input_tokens: usage.cached_content_token_count,
                reasoning_tokens: usage.thoughts_token_count,
                ..CanonicalUsage::default()
            })
        })
        .transpose()
}
