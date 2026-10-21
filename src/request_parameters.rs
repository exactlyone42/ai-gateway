use serde_json::{Map, Value};

use crate::model_registry::GatewayOperation;

const MAX_MODEL_BYTES: usize = 255;

pub fn snapshot(operation: GatewayOperation, document: &Value) -> Value {
    let mut result = Map::new();
    result.insert("schema_version".to_owned(), Value::from(1));
    result.insert(
        "operation".to_owned(),
        Value::String(operation.as_str().to_owned()),
    );
    if let Some(model) = document.get("model").and_then(Value::as_str) {
        result.insert(
            "model".to_owned(),
            Value::String(bounded(model, MAX_MODEL_BYTES)),
        );
    }
    copy_number(document, &mut result, "max_tokens", "max_output_tokens");
    copy_number(
        document,
        &mut result,
        "max_output_tokens",
        "max_output_tokens",
    );
    copy_number(document, &mut result, "temperature", "temperature");
    copy_number(document, &mut result, "top_p", "top_p");
    copy_number(document, &mut result, "top_k", "top_k");
    copy_bool(document, &mut result, "stream");
    copy_bool(document, &mut result, "parallel_tool_calls");

    let messages = document.get("messages").or_else(|| document.get("input"));
    let counts = messages.map(structural_counts).unwrap_or_default();
    result.insert("message_count".to_owned(), Value::from(counts.messages));
    result.insert("content_part_count".to_owned(), Value::from(counts.parts));
    result.insert("text_chars".to_owned(), Value::from(counts.text_chars));
    result.insert("image_count".to_owned(), Value::from(counts.images));
    result.insert(
        "tool_count".to_owned(),
        Value::from(
            document
                .get("tools")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        ),
    );
    result.insert(
        "has_system".to_owned(),
        Value::Bool(document.get("system").is_some() || document.get("instructions").is_some()),
    );
    result.insert(
        "has_structured_output".to_owned(),
        Value::Bool(
            document.get("response_format").is_some()
                || document.pointer("/text/format").is_some()
                || document.pointer("/output_config/format").is_some(),
        ),
    );
    if let Some(reasoning) = reasoning_snapshot(document) {
        result.insert("reasoning".to_owned(), reasoning);
    }
    Value::Object(result)
}

fn copy_number(
    source: &Value,
    target: &mut Map<String, Value>,
    source_key: &str,
    target_key: &str,
) {
    if let Some(value) = source.get(source_key).filter(|value| value.is_number()) {
        target.insert(target_key.to_owned(), value.clone());
    }
}

fn copy_bool(source: &Value, target: &mut Map<String, Value>, key: &str) {
    if let Some(value) = source.get(key).and_then(Value::as_bool) {
        target.insert(key.to_owned(), Value::Bool(value));
    }
}

fn reasoning_snapshot(document: &Value) -> Option<Value> {
    let value = document
        .get("thinking")
        .or_else(|| document.get("reasoning"))?;
    let mut result = Map::new();
    if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
        result.insert("enabled".to_owned(), Value::Bool(enabled));
    }
    if let Some(budget) = value.get("budget_tokens").and_then(Value::as_u64) {
        result.insert("budget_tokens".to_owned(), Value::from(budget));
    }
    for key in ["type", "effort"] {
        if let Some(value) = value.get(key).and_then(Value::as_str).filter(|value| {
            matches!(
                *value,
                "adaptive"
                    | "enabled"
                    | "disabled"
                    | "none"
                    | "minimal"
                    | "low"
                    | "medium"
                    | "high"
                    | "xhigh"
                    | "max"
            )
        }) {
            result.insert(key.to_owned(), Value::String(value.to_owned()));
        }
    }
    (!result.is_empty()).then_some(Value::Object(result))
}

#[derive(Default)]
struct Counts {
    messages: u64,
    parts: u64,
    text_chars: u64,
    images: u64,
}

fn structural_counts(value: &Value) -> Counts {
    let mut counts = Counts::default();
    match value {
        Value::String(text) => {
            counts.messages = 1;
            counts.parts = 1;
            counts.text_chars = text.chars().count() as u64;
        }
        Value::Array(messages) => {
            counts.messages = messages.len() as u64;
            for message in messages {
                count_content(message.get("content").unwrap_or(message), &mut counts);
            }
        }
        Value::Object(_) => {
            counts.messages = 1;
            count_content(value.get("content").unwrap_or(value), &mut counts);
        }
        _ => {}
    }
    counts
}

fn count_content(value: &Value, counts: &mut Counts) {
    match value {
        Value::String(text) => {
            counts.parts += 1;
            counts.text_chars += text.chars().count() as u64;
        }
        Value::Array(items) => {
            for item in items {
                count_content(item, counts);
            }
        }
        Value::Object(object) => {
            counts.parts += 1;
            if object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "image" | "image_url" | "input_image"))
            {
                counts.images += 1;
            }
            if let Some(text) = object.get("text").and_then(Value::as_str) {
                counts.text_chars += text.chars().count() as u64;
            } else if let Some(content) = object.get("content") {
                count_content(content, counts);
            }
        }
        _ => {}
    }
}

fn bounded(value: &str, max_bytes: usize) -> String {
    let mut boundary = value.len().min(max_bytes);
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn snapshot_keeps_parameters_and_only_content_sizes() {
        let snapshot = snapshot(
            GatewayOperation::AnthropicMessages,
            &json!({
                "model": "public-model",
                "max_tokens": 4096,
                "temperature": 0.2,
                "stream": true,
                "thinking": {"type":"adaptive", "secret":"sentinel-secret"},
                "system": "sentinel-system",
                "messages": [{"role":"user","content":[
                    {"type":"text","text":"sentinel-prompt"},
                    {"type":"image","source":{"data":"sentinel-image"}}
                ]}],
                "tools": [{"name":"sentinel-tool","description":"sentinel-description"}]
            }),
        );
        assert_eq!(snapshot["max_output_tokens"], 4096);
        assert_eq!(snapshot["message_count"], 1);
        assert_eq!(snapshot["tool_count"], 1);
        assert_eq!(snapshot["image_count"], 1);
        assert_eq!(snapshot["text_chars"], 15);
        assert_eq!(snapshot["reasoning"]["type"], "adaptive");
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded.contains("sentinel"));
    }
}
