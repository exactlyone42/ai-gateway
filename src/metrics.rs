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

use std::sync::OnceLock;

use opentelemetry::{
    KeyValue,
    metrics::{Histogram, UpDownCounter},
};
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::{
    Resource,
    metrics::{PeriodicReader, SdkMeterProvider},
};

const SERVICE_NAME: &str = "agent-pool-gateway";

/// Installs the meter provider when an endpoint is configured, and returns it
/// so shutdown can flush. Without an endpoint the instruments below bind to
/// the global no-op meter: recording still compiles and costs nothing.
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

/// One request's worth of measurement.
///
/// Only bounded attributes are attached. Request ids, trace ids and model
/// names chosen by a caller would each multiply the time series; those belong
/// in logs and traces, and `./scripts/check.sh bans` fails a build that adds
/// one here.
pub fn record(method: &str, route: &str, status: u16, error_type: &str, duration: f64) {
    let mut attributes = vec![
        KeyValue::new("http.request.method", method.to_owned()),
        KeyValue::new("http.response.status_code", i64::from(status)),
    ];
    // An unmatched path has no route, and inventing one would turn a bounded
    // attribute into an unbounded one.
    if !route.is_empty() {
        attributes.push(KeyValue::new("http.route", route.to_owned()));
    }
    if !error_type.is_empty() {
        attributes.push(KeyValue::new("error.type", error_type.to_owned()));
    }
    instruments().duration.record(duration, &attributes);
}

/// Counts what is in flight for as long as it is alive.
///
/// A guard rather than a start/stop pair: a client that disconnects mid-request
/// makes the handler future be dropped, and a decrement written as a statement
/// after `await` would never run -- the gauge would then climb forever and
/// read as a stuck gateway.
pub struct InFlight;

impl InFlight {
    pub fn new() -> Self {
        instruments().active_requests.add(1, &[]);
        Self
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        instruments().active_requests.add(-1, &[]);
    }
}

struct Instruments {
    duration: Histogram<f64>,
    active_requests: UpDownCounter<i64>,
}

/// Built on first use, which is after `init()` has set the global provider --
/// instruments created earlier would stay bound to the no-op meter.
fn instruments() -> &'static Instruments {
    static INSTRUMENTS: OnceLock<Instruments> = OnceLock::new();
    INSTRUMENTS.get_or_init(|| {
        let meter = opentelemetry::global::meter_provider().meter(SERVICE_NAME);
        Instruments {
            duration: meter
                .f64_histogram("http.server.request.duration")
                .with_unit("s")
                .with_description("Duration of HTTP server requests.")
                .build(),
            active_requests: meter
                .i64_up_down_counter("http.server.active_requests")
                .with_unit("{request}")
                .with_description("Number of active HTTP server requests.")
                .build(),
        }
    })
}
