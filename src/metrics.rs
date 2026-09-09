//! Metrics: the numbers that answer "is the gateway healthy right now".
//!
//! Logs answer "what happened to this one request"; they cannot answer
//! "how many, how slow, how often failing" without someone aggregating them,
//! and aggregating log text makes the answer depend on log wording.
//!
//! Names come from the OpenTelemetry semantic conventions
//! (`opentelemetry-semantic-conventions-0.27.0/src/metric.rs:1589,1642`).
//! The spec's §57 list of 13 metrics is not in this repository, so this is
//! the semconv-defined subset that the current endpoints can actually
//! produce -- see `docs/observability.md` for what is still missing.

use std::time::Instant;

use opentelemetry::{
    KeyValue,
    metrics::{Histogram, Meter, UpDownCounter},
};
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::{
    Resource,
    metrics::{PeriodicReader, SdkMeterProvider},
};

const SERVICE_NAME: &str = "agent-pool-gateway";

/// `error.type` for a request whose caller went away. A bounded value, so it
/// is safe as an attribute, and it keeps "nobody waited for the answer"
/// distinguishable from "we answered with an error".
const CANCELLED: &str = "cancelled";

/// Bucket boundaries in **seconds**, from the HTTP semantic conventions.
///
/// The SDK default is `[0, 5, 10, 25, ...]`
/// (`opentelemetry_sdk-0.32.1/src/metrics/pipeline.rs:513`), which is a
/// millisecond-shaped ladder: with it every request under five seconds lands
/// in the first bucket and p50, p95 and p99 all read the same number --
/// destroying the only reason to keep a histogram.
const DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// Installs the meter provider when an endpoint is configured, and returns it
/// so shutdown can flush. Without an endpoint the instruments bind to the
/// global no-op meter: recording still compiles and costs nothing.
pub fn init(otlp_endpoint: Option<&str>) -> Option<SdkMeterProvider> {
    let endpoint = otlp_endpoint?;
    let exporter = match opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(format!("{endpoint}/v1/metrics"))
        .build()
    {
        Ok(exporter) => exporter,
        Err(error) => {
            tracing::error!(
                error.type = "otlp_metric_exporter_unavailable",
                exception.message = %error,
                "metrics are not exported; the gateway keeps serving"
            );
            return None;
        }
    };

    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter).build())
        .with_resource(Resource::builder().with_service_name(SERVICE_NAME).build())
        .build();
    opentelemetry::global::set_meter_provider(provider.clone());
    Some(provider)
}

/// The instruments, held in `AppState`.
///
/// Deliberately not a global: an instrument binds to whichever provider is
/// installed when it is built, so "built after `init()`" has to be visible in
/// the code rather than depend on which function happens to run first.
pub struct Instruments {
    duration: Histogram<f64>,
    active_requests: UpDownCounter<i64>,
}

impl Instruments {
    /// Binds to the meter provider installed right now.
    pub fn new() -> Self {
        Self::from_meter(opentelemetry::global::meter_provider().meter(SERVICE_NAME))
    }

    /// Lets a test bind to its own reader instead of the global provider.
    pub fn from_meter(meter: Meter) -> Self {
        Self {
            duration: meter
                .f64_histogram("http.server.request.duration")
                .with_unit("s")
                .with_description("Duration of HTTP server requests.")
                .with_boundaries(DURATION_BUCKETS.to_vec())
                .build(),
            active_requests: meter
                .i64_up_down_counter("http.server.active_requests")
                .with_unit("{request}")
                .with_description("Number of active HTTP server requests.")
                .build(),
        }
    }

    /// Starts measuring one request. The returned scope counts as in flight
    /// until it is dropped.
    pub fn scope(&self, method: &str, route: &str) -> RequestScope<'_> {
        self.active_requests.add(1, &[]);
        RequestScope {
            instruments: self,
            method: method.to_owned(),
            route: route.to_owned(),
            started: Instant::now(),
            completed: false,
        }
    }

    /// Only bounded attributes are attached. Request ids, trace ids and
    /// caller-chosen model names would each multiply the stored time series;
    /// those belong in logs and traces, and `./scripts/check.sh bans` fails a
    /// build that adds one here.
    fn record(
        &self,
        method: &str,
        route: &str,
        status: Option<u16>,
        error_type: &str,
        seconds: f64,
    ) {
        let mut attributes = vec![KeyValue::new("http.request.method", method.to_owned())];
        if let Some(status) = status {
            attributes.push(KeyValue::new(
                "http.response.status_code",
                i64::from(status),
            ));
        }
        // An unmatched path has no route, and inventing one would turn a
        // bounded attribute into an unbounded one.
        if !route.is_empty() {
            attributes.push(KeyValue::new("http.route", route.to_owned()));
        }
        if !error_type.is_empty() {
            attributes.push(KeyValue::new("error.type", error_type.to_owned()));
        }
        self.duration.record(seconds, &attributes);
    }
}

/// One request's measurement, ended either by `complete()` or by being
/// dropped.
///
/// A guard rather than plain statements after `await`: a caller that
/// disconnects mid-request makes the handler future be dropped, and anything
/// written after the await -- the decrement, the histogram sample, the access
/// log line -- would simply never run. The gauge would climb forever and the
/// request would leave no trace at all.
pub struct RequestScope<'a> {
    instruments: &'a Instruments,
    method: String,
    route: String,
    started: Instant,
    completed: bool,
}

impl RequestScope<'_> {
    /// Records the request and returns its duration in seconds, so the log
    /// line and the histogram cannot disagree about the number.
    pub fn complete(&mut self, status: u16, error_type: &str) -> f64 {
        let seconds = self.started.elapsed().as_secs_f64();
        self.instruments
            .record(&self.method, &self.route, Some(status), error_type, seconds);
        self.completed = true;
        seconds
    }
}

impl Drop for RequestScope<'_> {
    fn drop(&mut self) {
        self.instruments.active_requests.add(-1, &[]);
        if self.completed {
            return;
        }
        // No status: there was no response. Recording it anyway keeps
        // "requests started" and "requests measured" from drifting apart,
        // which is the difference between a quiet gateway and a gateway
        // whose callers all hang up.
        let seconds = self.started.elapsed().as_secs_f64();
        self.instruments
            .record(&self.method, &self.route, None, CANCELLED, seconds);
        tracing::warn!(
            event.name = "http.access",
            http.request.method = %self.method,
            http.route = %self.route,
            http.server.request.duration = seconds,
            error.type = CANCELLED,
            "the caller went away before a response was produced"
        );
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::{
        InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
        data::{AggregatedMetrics, MetricData},
    };

    use super::{CANCELLED, DURATION_BUCKETS, Instruments};

    /// Reads the instruments through a real SDK pipeline, so a wrong unit, a
    /// wrong bucket ladder or a missing attribute shows up as a failing
    /// assertion rather than as a chart nobody checks.
    fn collected(record: impl FnOnce(&Instruments)) -> (InMemoryMetricExporter, SdkMeterProvider) {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        let instruments = Instruments::from_meter(provider.meter("test"));

        record(&instruments);
        provider.force_flush().unwrap();
        (exporter, provider)
    }

    /// Every duration data point as `(attributes, bucket bounds)`.
    fn duration_points(exporter: &InMemoryMetricExporter) -> Vec<(Vec<String>, Vec<f64>)> {
        let mut points = Vec::new();
        for batch in exporter.get_finished_metrics().unwrap() {
            for scope in batch.scope_metrics() {
                for metric in scope.metrics() {
                    if metric.name() != "http.server.request.duration" {
                        continue;
                    }
                    assert_eq!(metric.unit(), "s", "the duration must be in seconds");
                    if let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data()
                    {
                        for point in histogram.data_points() {
                            let attributes = point
                                .attributes()
                                .map(|attribute| {
                                    format!("{}={}", attribute.key, attribute.value.as_str())
                                })
                                .collect();
                            points.push((attributes, point.bounds().collect()));
                        }
                    }
                }
            }
        }
        points
    }

    /// The SDK default ladder starts at 5, which in seconds means every
    /// request under five seconds shares one bucket and all percentiles read
    /// the same number.
    #[test]
    fn duration_buckets_are_in_seconds() {
        let (exporter, _provider) = collected(|instruments| {
            let mut scope = instruments.scope("GET", "/healthz");
            scope.complete(200, "");
        });

        let points = duration_points(&exporter);
        let (attributes, bounds) = points.first().expect("no duration data point");
        assert_eq!(bounds, &DURATION_BUCKETS.to_vec(), "wrong bucket ladder");
        assert!(
            bounds.first().is_some_and(|first| *first < 0.01),
            "the first bucket must resolve fast requests: {bounds:?}"
        );
        assert!(
            attributes.contains(&"http.route=/healthz".to_owned()),
            "missing route attribute: {attributes:?}"
        );
    }

    /// A request whose caller went away must still be measured *and* still
    /// leave a log line, or "requests started" and "requests measured" drift
    /// apart with nothing to explain the gap.
    #[test]
    fn a_dropped_scope_is_still_recorded_as_cancelled() {
        // The real subscriber, not a stock one: a test that asserts against a
        // format the binary does not emit proves nothing.
        let buffer = crate::logging::capture::Buffer::default();
        let (subscriber, _traces) = crate::logging::test_subscriber(buffer.clone());

        let (exporter, _provider) = collected(|instruments| {
            tracing::subscriber::with_default(subscriber, || {
                drop(instruments.scope("POST", "/v1/chat/completions"));
            });
        });

        let logged = buffer.lines();
        assert!(
            logged.iter().any(|line| {
                line["error"]["type"] == CANCELLED && line["event"]["name"] == "http.access"
            }),
            "a cancelled request left no access log line: {logged:?}"
        );

        let cancelled: Vec<Vec<String>> = duration_points(&exporter)
            .into_iter()
            .map(|(attributes, _)| attributes)
            .filter(|attributes| attributes.iter().any(|pair| pair == "error.type=cancelled"))
            .collect();
        assert_eq!(cancelled.len(), 1, "expected one cancelled sample");
        assert!(
            !cancelled[0]
                .iter()
                .any(|pair| pair.starts_with("http.response.status_code")),
            "a cancelled request has no status: {:?}",
            cancelled[0]
        );
    }
}
