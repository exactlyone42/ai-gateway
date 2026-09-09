//! The log wire format: one JSON object per line, OpenTelemetry naming.
//!
//! Why not the stock `.json()` formatter: it always writes `level` as a
//! string (`tracing-subscriber-0.3.23/src/fmt/format/json.rs:250`, no option
//! changes it), while every consumer we have filters on a numeric severity.
//! Shape is the only thing this module decides -- field names come from the
//! OpenTelemetry semantic conventions and are written at the call site
//! verbatim (`http.route`, `http.response.status_code`,
//! `http.server.request.duration` in seconds, `error.type`).
//!
//! Dotted names arrive flat from `tracing` and are nested here, because a
//! consumer addressing a field by JSON path (`{ $.http.response.status_code
//! >= 500 }`) cannot address a literal dotted key.

use std::fmt;

use serde_json::{Map, Value};
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    EnvFilter,
    fmt::{
        FmtContext, FormatEvent, FormatFields,
        format::Writer,
        time::{FormatTime, SystemTime},
    },
    registry::LookupSpan,
};

/// Installs the subscriber. Which levels pass still comes from `RUST_LOG`.
pub fn init() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .event_format(OtelJson)
        .init();
}

/// Formats an event as an OpenTelemetry-shaped log record.
///
/// Only the event's own fields are read. Span fields are deliberately not
/// merged: the one place that knows a whole request -- the access log layer --
/// emits a single event carrying everything, so there is nothing to inherit
/// and no chance of the same key landing twice.
struct OtelJson;

impl<S, N> FormatEvent<S, N> for OtelJson
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _context: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let metadata = event.metadata();
        let (severity_number, severity_text) = severity(metadata.level());

        let mut record = Map::new();
        record.insert("timestamp".to_owned(), Value::from(now()));
        record.insert("severity_number".to_owned(), Value::from(severity_number));
        record.insert("severity_text".to_owned(), Value::from(severity_text));
        record.insert("target".to_owned(), Value::from(metadata.target()));
        event.record(&mut Collector {
            record: &mut record,
        });

        let line = serde_json::to_string(&record).map_err(|_| fmt::Error)?;
        writeln!(writer, "{line}")
    }
}

/// Severity numbers from the OTLP log data model: TRACE 1, DEBUG 5, INFO 9,
/// WARN 13, ERROR 17 (`opentelemetry-proto-0.27.0`,
/// `src/proto/tonic/opentelemetry.proto.logs.v1.rs:189-214`).
fn severity(level: &Level) -> (u8, &'static str) {
    match *level {
        Level::TRACE => (1, "TRACE"),
        Level::DEBUG => (5, "DEBUG"),
        Level::INFO => (9, "INFO"),
        Level::WARN => (13, "WARN"),
        Level::ERROR => (17, "ERROR"),
    }
}

/// Writing into a `String` cannot fail in practice; an empty timestamp is
/// still preferable to dropping the record, and it is visibly wrong rather
/// than plausibly wrong.
fn now() -> String {
    let mut buffer = String::new();
    let _ = SystemTime.format_time(&mut Writer::new(&mut buffer));
    buffer
}

struct Collector<'a> {
    record: &'a mut Map<String, Value>,
}

impl Collector<'_> {
    fn put(&mut self, field: &Field, value: Value) {
        // An empty string means the call site had nothing for this field, and
        // a missing value must stay missing: `""` reads like "this request
        // had no route" rather than "we could not tell".
        if value.as_str().is_some_and(str::is_empty) {
            return;
        }
        // An event's message is the log record's body in OTel terms.
        let name = match field.name() {
            "message" => "body",
            other => other,
        };
        insert_nested(self.record, name, value);
    }
}

impl Visit for Collector<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, Value::from(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, Value::from(value));
    }

    // `%value` (Display) and `?value` (Debug) both land here.
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.put(field, Value::from(format!("{value:?}")));
    }
}

/// Places `a.b.c` at `{"a":{"b":{"c":...}}}`.
///
/// A non-object already sitting on the path is replaced: losing a field
/// entirely is worse than losing a value whose key was already ambiguous.
fn insert_nested(record: &mut Map<String, Value>, name: &str, value: Value) {
    match name.split_once('.') {
        None => {
            record.insert(name.to_owned(), value);
        }
        Some((head, rest)) => {
            let child = record
                .entry(head.to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            if !child.is_object() {
                *child = Value::Object(Map::new());
            }
            if let Some(object) = child.as_object_mut() {
                insert_nested(object, rest, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    use serde_json::Value;
    use tracing_subscriber::fmt::MakeWriter;

    use super::OtelJson;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Buffer {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl MakeWriter<'_> for Buffer {
        type Writer = Self;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    /// Emits through the real subscriber and parses what came out, so a test
    /// fails when the wire format changes -- not when a helper changes.
    fn emitted(emit: impl FnOnce()) -> Value {
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .event_format(OtelJson)
            .with_writer(buffer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, emit);
        let bytes = buffer.0.lock().unwrap().clone();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn severity_is_a_number_not_a_string() {
        let line = emitted(|| tracing::error!("upstream request failed"));

        assert_eq!(line["severity_number"], Value::from(17));
        assert_eq!(line["severity_text"], Value::from("ERROR"));
        assert_eq!(line["body"], Value::from("upstream request failed"));
        assert!(
            line["timestamp"].as_str().is_some_and(|t| t.ends_with('Z')),
            "timestamp must be a UTC instant: {line}"
        );
    }

    #[test]
    fn warn_maps_to_thirteen() {
        let line = emitted(|| tracing::warn!("model is not allowed by policy"));

        assert_eq!(line["severity_number"], Value::from(13));
        assert_eq!(line["severity_text"], Value::from("WARN"));
    }

    #[test]
    fn semantic_convention_names_become_nested_objects() {
        let line = emitted(|| {
            tracing::error!(
                http.route = "/v1/chat/completions",
                http.request.method = "POST",
                http.response.status_code = 502_u64,
                http.server.request.duration = 0.812_f64,
                error.type = "upstream_unavailable",
                request_id = %"abc-123",
                "upstream request failed"
            )
        });

        assert_eq!(line["http"]["route"], Value::from("/v1/chat/completions"));
        assert_eq!(line["http"]["request"]["method"], Value::from("POST"));
        assert_eq!(line["http"]["response"]["status_code"], Value::from(502));
        assert_eq!(
            line["http"]["server"]["request"]["duration"],
            Value::from(0.812)
        );
        assert_eq!(line["error"]["type"], Value::from("upstream_unavailable"));
        // Display-valued fields must not arrive quoted.
        assert_eq!(line["request_id"], Value::from("abc-123"));
    }
}
