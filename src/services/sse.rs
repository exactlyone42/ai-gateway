use serde_json::Value;

use crate::ledger::TokenUsage;

const MAX_EVENT_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub struct MetadataParser {
    line: Vec<u8>,
    data: Vec<u8>,
    overflowed: bool,
    response_id: Option<String>,
    usage: TokenUsage,
    terminal: bool,
    after_cr: bool,
}

pub struct Observation {
    pub changed: bool,
    pub response_id: Option<String>,
    pub usage: TokenUsage,
    pub terminal: bool,
}

impl MetadataParser {
    /// Observes complete SSE events while leaving the caller's raw bytes
    /// untouched. Bad metadata is ignored until the next event boundary.
    pub fn feed(&mut self, bytes: &[u8]) -> Observation {
        let mut changed = false;
        for byte in bytes {
            if self.after_cr {
                self.after_cr = false;
                if *byte == b'\n' {
                    continue;
                }
            }
            match *byte {
                b'\r' => {
                    changed |= self.finish_line();
                    self.after_cr = true;
                }
                b'\n' => changed |= self.finish_line(),
                byte if self.line.len() < MAX_EVENT_BYTES => self.line.push(byte),
                _ => {
                    self.overflowed = true;
                    self.line.clear();
                    self.data.clear();
                }
            }
        }
        Observation {
            changed,
            response_id: self.response_id.clone(),
            usage: self.usage.clone(),
            terminal: self.terminal,
        }
    }

    fn finish_line(&mut self) -> bool {
        if self.line.is_empty() {
            let changed = if self.overflowed {
                false
            } else {
                self.finish_event()
            };
            self.data.clear();
            self.overflowed = false;
            return changed;
        }
        if !self.overflowed && self.line.starts_with(b"data:") {
            let mut value = &self.line[5..];
            if value.first() == Some(&b' ') {
                value = &value[1..];
            }
            let separator = usize::from(!self.data.is_empty());
            if self.data.len() + separator + value.len() <= MAX_EVENT_BYTES {
                if separator == 1 {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(value);
            } else {
                self.overflowed = true;
                self.data.clear();
            }
        }
        self.line.clear();
        false
    }

    fn finish_event(&mut self) -> bool {
        if self.data.is_empty() {
            return false;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&self.data) else {
            return false;
        };
        let mut changed = false;
        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(id) = value
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                {
                    changed |= self.response_id.as_ref() != Some(&id);
                    self.response_id = Some(id);
                }
                if let Some(usage) = value.pointer("/message/usage") {
                    changed |= merge_usage(&mut self.usage, usage);
                }
            }
            Some("message_delta") => {
                if let Some(usage) = value.get("usage") {
                    changed |= merge_usage(&mut self.usage, usage);
                }
            }
            Some("message_stop") => {
                changed |= !self.terminal;
                self.terminal = true;
            }
            _ => {}
        }
        changed
    }
}

fn merge_usage(current: &mut TokenUsage, value: &Value) -> bool {
    let before = current.clone();
    set_if_present(value, "input_tokens", &mut current.input_tokens);
    set_if_present(value, "output_tokens", &mut current.output_tokens);
    set_if_present(
        value,
        "cache_read_input_tokens",
        &mut current.cached_input_tokens,
    );
    set_if_present(
        value,
        "cache_creation_input_tokens",
        &mut current.cache_write_input_tokens,
    );
    *current != before
}

fn set_if_present(value: &Value, key: &str, target: &mut u64) {
    if let Some(next) = value.get(key).and_then(Value::as_u64) {
        *target = next;
    }
}

#[cfg(test)]
mod tests {
    use super::MetadataParser;

    #[test]
    fn parses_crlf_multiline_and_split_frames() {
        let raw = concat!(
            ": ping\r\n\r\n",
            "data: {\"type\":\"message_start\",\r\n",
            "data: \"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":7}}}\r\n\r\n",
            "data: {\"type\":\"message_stop\"}\r\n\r\n"
        )
        .as_bytes();
        let split = raw.len() / 2;
        let mut parser = MetadataParser::default();
        assert!(!parser.feed(&raw[..split]).terminal);
        let result = parser.feed(&raw[split..]);
        assert_eq!(result.response_id.as_deref(), Some("msg_1"));
        assert_eq!(result.usage.input_tokens, 7);
        assert!(result.terminal);
    }

    #[test]
    fn oversized_unknown_event_does_not_hide_terminal_usage() {
        let mut bytes = b"data: ".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', 70 * 1024));
        bytes.extend_from_slice(b"\n\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":9}}\n\ndata: {\"type\":\"message_stop\"}\n\n");
        let result = MetadataParser::default().feed(&bytes);
        assert_eq!(result.usage.output_tokens, 9);
        assert!(result.terminal);
    }

    #[test]
    fn parses_cr_only_event_boundaries() {
        let raw = b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_cr\",\"usage\":{\"input_tokens\":3}}}\r\rdata: {\"type\":\"message_stop\"}\r\r";
        let mut parser = MetadataParser::default();
        let result = parser.feed(raw);
        assert_eq!(result.response_id.as_deref(), Some("msg_cr"));
        assert_eq!(result.usage.input_tokens, 3);
        assert!(result.terminal);
    }
}
