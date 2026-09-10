use std::sync::Arc;

use axum::{
    extract::{MatchedPath, Request, State},
    middleware::Next,
    response::Response,
};

use tracing::Instrument as _;
use tracing_opentelemetry::{OpenTelemetrySpanExt as _, SetParentError};

use crate::{
    auth,
    error::{ErrorType, GatewayError},
    identity::PrincipalContext,
    logging,
    state::AppState,
};

/// Turns a request away before it reaches any handler unless it carries a
/// valid client key.
pub async fn require_client_key(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Result<Response, GatewayError> {
    let principal = auth::authorize(&state, request.headers()).await?;
    request.extensions_mut().insert(principal.clone());
    let mut response = next.run(request).await;
    response.extensions_mut().insert(principal);
    Ok(response)
}

/// Request id header, set by the layer above and read back here.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Writes one log record per request -- the only place that does.
///
/// It runs inside the request-id layer and outside everything else, so it sees
/// the id, the final status (including a rejection from the layer below) and
/// the whole duration. `error.type` arrives from the error boundary through a
/// response extension; a status with no gateway error of its own (a 404 from
/// the router, a body the extractor rejected) falls back to the status code,
/// which is what the HTTP semantic conventions prescribe.
pub async fn access_log(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    // The span is what gives the request its trace id; everything below runs
    // inside it, so the handlers' own spans become its children. If the caller
    // sent a `traceparent`, this span joins that trace instead of starting a
    // new one -- otherwise a request that crosses two services looks like two
    // unrelated requests.
    let parent = logging::parent_context(request.headers());
    let span = tracing::info_span!("gateway.request");
    // `LayerNotFound` and `SpanDisabled` mean tracing is simply not
    // installed or not sampled -- both are deployment choices, and logging
    // them would put a line on every request. Anything else is a real
    // failure to continue the caller's trace and must not pass silently.
    if let Err(error) = span.set_parent(parent)
        && !matches!(
            error,
            SetParentError::LayerNotFound | SetParentError::SpanDisabled
        )
    {
        tracing::warn!(
            error.type = "trace_parent_not_applied",
            exception.message = ?error,
            "the caller's trace was not continued"
        );
    }
    record(state, request, next).instrument(span).await
}

async fn record(state: Arc<AppState>, request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    // Absent before routing happens, and absent for a path that matches no
    // route at all. Empty means unknown and is dropped, never guessed.
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_default();

    // Measurement starts here and ends when the scope is dropped, so a
    // cancelled request is still counted -- see `metrics::RequestScope`.
    let mut scope = state.instruments.scope(method.as_str(), &route);

    let response = next.run(request).await;

    let status = response.status();
    let failure = response.extensions().get::<ErrorType>().cloned();
    let error_type = failure
        .as_ref()
        .map(|failure| failure.kind.to_owned())
        .unwrap_or_else(|| {
            if status.is_client_error() || status.is_server_error() {
                status.as_u16().to_string()
            } else {
                String::new()
            }
        });
    let cause = failure
        .and_then(|failure| failure.cause)
        .unwrap_or_default();
    let principal = response
        .extensions()
        .get::<PrincipalContext>()
        .map(PrincipalContext::log_id)
        .unwrap_or_default();

    // One number for both the histogram and the log line.
    let duration = scope.complete(status.as_u16(), &error_type);

    let (trace_id, span_id) = logging::current_trace_ids();

    // The level has to be known at compile time (`tracing`'s callsite is a
    // `static`), so the field list is shared through a local macro instead of
    // being written three times.
    macro_rules! emit {
        ($level:ident) => {
            tracing::$level!(
                event.name = "http.access",
                request_id = %request_id,
                trace_id = %trace_id,
                span_id = %span_id,
                principal = %principal,
                http.request.method = %method,
                http.route = %route,
                url.path = %path,
                http.response.status_code = status.as_u16(),
                http.server.request.duration = duration,
                error.type = %error_type,
                exception.message = %cause,
                "request completed"
            )
        };
    }

    if status.is_server_error() {
        emit!(error);
    } else if status.is_client_error() {
        emit!(warn);
    } else {
        emit!(info);
    }

    response
}
