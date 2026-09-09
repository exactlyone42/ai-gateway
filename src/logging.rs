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

use opentelemetry::trace::{TraceContextExt, TracerProvider as _};
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use serde_json::{Map, Value};
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{
    EnvFilter,
    fmt::{
        FmtContext, FormatEvent, FormatFields,
        format::Writer,
        time::{FormatTime, SystemTime},
    },
    layer::SubscriberExt,
    registry::LookupSpan,
    util::SubscriberInitExt,
};

const SERVICE_NAME: &str = "agent-pool-gateway";

/// Installs the subscriber, plus the OTLP span exporter when an endpoint is
/// configured. Which levels pass still comes from `RUST_LOG`.
///
/// Returns the provider so shutdown can flush it. Without an endpoint there
/// is no provider and no exporter: spans still exist and still carry ids for
/// the log records, they are just not sent anywhere.
pub fn init(otlp_endpoint: Option<&str>) -> Option<SdkTracerProvider> {
    // The exporter is built before the subscriber exists, so a failure here
    // cannot be logged yet -- it is carried out and logged below.
    let (provider, failure) = match otlp_endpoint {
        None => (None, None),
        Some(endpoint) => match provider_for(endpoint) {
            Ok(provider) => (Some(provider), None),
            Err(error) => (None, Some(error.to_string())),
        },
    };

    let traces = provider
        .as_ref()
        .map(|provider| tracing_opentelemetry::layer().with_tracer(provider.tracer(SERVICE_NAME)));

    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().event_format(OtelJson))
        .with(traces)
        .init();

    install_panic_logging();

    if let Some(reason) = failure {
        tracing::error!(
            error.type = "otlp_exporter_unavailable",
            exception.message = %reason,
            "traces are not exported; the gateway keeps serving"
        );
    }
    provider
}

/// Turns a panic into one JSON log line.
///
/// Measured 2026-09-09: the OTLP batch thread panicked and wrote a bare
/// multi-line message to stderr, which is not JSON -- a single such line makes
/// a log consumer fall back to text parsing for the whole source. A panic in a
/// dependency is not hypothetical, so the stream has to survive one.
fn install_panic_logging() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_default();
        tracing::error!(
            error.type = "panic",
            exception.message = %info.payload_as_str().unwrap_or("<non-string payload>"),
            code.filepath = %location,
            "a thread panicked"
        );
    }));
}

/// A programmatic endpoint is used verbatim by the exporter, so the signal
/// path has to be appended here (`opentelemetry-otlp-0.32.0`,
/// `src/exporter/http/mod.rs:726`).
fn provider_for(
    endpoint: &str,
) -> Result<SdkTracerProvider, opentelemetry_otlp::ExporterBuildError> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(format!("{endpoint}/v1/traces"))
        .build()?;

    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(Resource::builder().with_service_name(SERVICE_NAME).build())
        .build())
}

/// Ids of the span the caller is inside, or empty strings when there is no
/// OpenTelemetry context.
///
/// Empty, not a placeholder: the formatter drops empty fields, so "this
/// request has no trace" stays visibly absent instead of looking like an id
/// that happens to be zero.
pub fn current_trace_ids() -> (String, String) {
    let context = tracing::Span::current().context();
    let span = context.span();
    let span_context = span.span_context();
    if span_context.is_valid() {
        (
            span_context.trace_id().to_string(),
            span_context.span_id().to_string(),
        )
    } else {
        (String::new(), String::new())
    }
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
        // Events from dependencies carry no message, only fields -- the OTel
        // SDK names its own events with a `name` field. Without this, an
        // export failure arrives with `body: null` (measured 2026-09-09).
        if !record.contains_key("body")
            && let Some(name) = record.get("name").cloned()
        {
            record.insert("body".to_owned(), name);
        }

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

/// Log capture for tests, shared with the router's integration test: a log
/// line is only proven to carry a field if a test reads the emitted bytes.
#[cfg(test)]
pub(crate) mod capture {
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    use serde_json::Value;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    pub(crate) struct Buffer(pub(crate) Arc<Mutex<Vec<u8>>>);

    impl Buffer {
        /// Every line emitted so far, parsed. Panics on malformed output,
        /// which is itself the thing under test.
        pub(crate) fn lines(&self) -> Vec<Value> {
            let bytes = self.0.lock().unwrap().clone();
            String::from_utf8(bytes)
                .unwrap()
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
    }

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
}

/// The same subscriber the binary installs, writing into a buffer: traces
/// layer included, so a test can prove a log line really carries a trace id.
/// The provider comes back with it because the tracer only records while the
/// provider is alive.
#[cfg(test)]
pub(crate) fn test_subscriber(
    buffer: capture::Buffer,
) -> (impl tracing::Subscriber + Send + Sync, SdkTracerProvider) {
    use opentelemetry::trace::TracerProvider as _;

    let provider = SdkTracerProvider::builder().build();
    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .event_format(OtelJson)
                .with_writer(buffer),
        )
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer(SERVICE_NAME)));
    (subscriber, provider)
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use serde_json::Value;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::{OtelJson, SERVICE_NAME, capture::Buffer, current_trace_ids};

    /// Emits through the real subscriber and parses what came out, so a test
    /// fails when the wire format changes -- not when a helper changes.
    fn emitted(emit: impl FnOnce()) -> Value {
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .event_format(OtelJson)
            .with_writer(buffer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, emit);
        let mut lines = buffer.lines();
        assert_eq!(lines.len(), 1, "expected exactly one line: {lines:?}");
        lines.remove(0)
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

    #[test]
    fn a_span_has_trace_ids_when_traces_are_installed() {
        let provider = SdkTracerProvider::builder().build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer(SERVICE_NAME)));

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("gateway.request");
            let _entered = span.enter();
            let (trace_id, span_id) = current_trace_ids();

            assert_eq!(trace_id.len(), 32, "trace id was {trace_id:?}");
            assert_eq!(span_id.len(), 16, "span id was {span_id:?}");
            assert!(trace_id.chars().all(|digit| digit.is_ascii_hexdigit()));
            assert_ne!(trace_id, "0".repeat(32), "an all-zero id means invalid");
        });
    }

    /// Without the layer there is no trace to point at, and the answer must be
    /// empty rather than a zero id that looks like a real one.
    #[test]
    fn a_span_has_no_trace_ids_without_the_layer() {
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().event_format(OtelJson));

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("gateway.request");
            let _entered = span.enter();

            assert_eq!(current_trace_ids(), (String::new(), String::new()));
        });
    }
}
