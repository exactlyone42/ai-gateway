use serde_json::Value;

use crate::{
    canonical::CanonicalStreamEvent,
    ledger::TokenUsage,
    services::streaming::{ProtocolError, StreamObserver, event_data},
};

#[derive(Default)]
pub struct MetadataParser {
    usage: TokenUsage,
}

impl StreamObserver for MetadataParser {
    fn observe(&mut self, event: &[u8]) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
        let data = event_data(event);
        if !data.has_data {
            return Ok(Vec::new());
        }
        let Ok(value) = serde_json::from_slice::<Value>(&data.bytes) else {
            return Ok(Vec::new());
        };
        let mut events = Vec::new();
        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(id) = value
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                {
                    events.push(CanonicalStreamEvent::ResponseStarted(id));
                }
                if let Some(usage) = value.pointer("/message/usage") {
                    merge_anthropic_usage(&mut self.usage, usage);
                    events.push(CanonicalStreamEvent::Usage(self.usage.clone().into()));
                }
            }
            Some("message_delta") => {
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    events.push(CanonicalStreamEvent::OutputCompleted {
                        index: 0,
                        finish_reason: reason.to_owned(),
                    });
                }
                if let Some(usage) = value.get("usage") {
                    merge_anthropic_usage(&mut self.usage, usage);
                    events.push(CanonicalStreamEvent::Usage(self.usage.clone().into()));
                }
            }
            Some("message_stop") => events.push(CanonicalStreamEvent::ResponseCompleted),
            _ => {}
        }
        Ok(events)
    }
}

fn merge_anthropic_usage(current: &mut TokenUsage, value: &Value) {
    for (key, target) in [
        ("input_tokens", &mut current.input_tokens),
        ("output_tokens", &mut current.output_tokens),
        ("cache_read_input_tokens", &mut current.cached_input_tokens),
        (
            "cache_creation_input_tokens",
            &mut current.cache_write_input_tokens,
        ),
    ] {
        if let Some(next) = value.get(key).and_then(Value::as_u64) {
            *target = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MetadataParser;
    use crate::{
        canonical::CanonicalStreamEvent,
        services::streaming::{EventDecoder, StreamObserver},
    };

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
        let mut decoder = EventDecoder::default();
        let mut parser = MetadataParser::default();
        let mut canonical = Vec::new();
        for event in decoder.feed(&raw[..split]).unwrap() {
            canonical.extend(parser.observe(&event).unwrap());
        }
        for event in decoder.feed(&raw[split..]).unwrap() {
            canonical.extend(parser.observe(&event).unwrap());
        }
        assert!(canonical.contains(&CanonicalStreamEvent::ResponseStarted("msg_1".to_owned())));
        assert!(canonical.iter().any(|event| matches!(
            event,
            CanonicalStreamEvent::Usage(usage) if usage.input_tokens == 7
        )));
        assert!(canonical.contains(&CanonicalStreamEvent::ResponseCompleted));
    }

    #[test]
    fn malformed_unknown_metadata_does_not_hide_terminal_usage() {
        let raw = b"data: not-json\n\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":9}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
        let mut decoder = EventDecoder::default();
        let mut parser = MetadataParser::default();
        let canonical = decoder
            .feed(raw)
            .unwrap()
            .into_iter()
            .flat_map(|event| parser.observe(&event).unwrap())
            .collect::<Vec<_>>();
        assert!(canonical.iter().any(|event| matches!(
            event,
            CanonicalStreamEvent::Usage(usage) if usage.output_tokens == 9
        )));
        assert!(canonical.contains(&CanonicalStreamEvent::ResponseCompleted));
    }

    #[test]
    fn preserves_anthropic_stop_reason_before_response_completion() {
        let raw = b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
        let mut decoder = EventDecoder::default();
        let mut parser = MetadataParser::default();
        let canonical = decoder
            .feed(raw)
            .unwrap()
            .into_iter()
            .flat_map(|event| parser.observe(&event).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            canonical,
            vec![
                CanonicalStreamEvent::OutputCompleted {
                    index: 0,
                    finish_reason: "tool_use".to_owned(),
                },
                CanonicalStreamEvent::ResponseCompleted,
            ]
        );
    }

    #[test]
    fn oversized_unknown_event_does_not_hide_terminal_usage() {
        let mut raw = b"data: ".to_vec();
        raw.extend(std::iter::repeat_n(b'x', 70 * 1024));
        raw.extend_from_slice(b"\n\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":9}}\n\ndata: {\"type\":\"message_stop\"}\n\n");
        let mut decoder = EventDecoder::default();
        let mut parser = MetadataParser::default();
        let canonical = decoder
            .feed(&raw)
            .unwrap()
            .into_iter()
            .flat_map(|event| parser.observe(&event).unwrap())
            .collect::<Vec<_>>();
        assert!(canonical.iter().any(|event| matches!(
            event,
            CanonicalStreamEvent::Usage(usage) if usage.output_tokens == 9
        )));
        assert!(canonical.contains(&CanonicalStreamEvent::ResponseCompleted));
    }
}
