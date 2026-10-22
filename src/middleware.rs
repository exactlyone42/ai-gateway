use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use axum::{
    body::{Body, Bytes},
    extract::{MatchedPath, Request, State},
    middleware::Next,
    response::{IntoResponse as _, Response},
};

/// Rejects oversized headers and new data-plane work once draining begins.
/// Health endpoints stay reachable on the same listener until active work is
/// gone or the hard drain deadline closes every connection.
pub async fn process_gate(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let header_bytes = request
        .headers()
        .iter()
        .fold(0usize, |total, (name, value)| {
            total
                .saturating_add(name.as_str().len())
                .saturating_add(value.as_bytes().len())
                .saturating_add(4)
        });
    if header_bytes > state.config.request_header_max_bytes {
        return GatewayError::PayloadTooLarge.into_response();
    }

    if matches!(request.uri().path(), "/health" | "/healthz" | "/ready") {
        return next.run(request).await;
    }
    let Some(active) = state.lifecycle.admit() else {
        return GatewayError::ProcessDraining.into_response();
    };
    let response = next.run(request).await;
    let (parts, body) = response.into_parts();
    let body = LifecycleBody {
        inner: Box::pin(body.into_data_stream()),
        _active: active,
    };
    Response::from_parts(parts, Body::from_stream(body))
}
use chrono::{DateTime, Utc};
use futures_util::Stream;

use tracing::Instrument as _;
use tracing_opentelemetry::{OpenTelemetrySpanExt as _, SetParentError};

use crate::{
    auth,
    error::{ErrorType, GatewayError},
    identity::PrincipalContext,
    lifecycle::ActiveRequest,
    logging,
    metrics::RequestScope,
    routes::StreamingResponse,
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

#[derive(Clone, Copy)]
pub(crate) struct RequestReceivedAt(pub DateTime<Utc>);

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

async fn record(state: Arc<AppState>, mut request: Request, next: Next) -> Response {
    request
        .extensions_mut()
        .insert(RequestReceivedAt(Utc::now()));
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

    let (trace_id, span_id) = logging::current_trace_ids();
    let context = AccessContext {
        request_id,
        trace_id,
        span_id,
        principal,
        method,
        route,
        path,
        status,
        span: tracing::Span::current(),
    };

    if response.extensions().get::<StreamingResponse>().is_some() {
        let (parts, body) = response.into_parts();
        let stream = TrackedBody {
            inner: Box::pin(body.into_data_stream()),
            scope: Some(scope),
            context,
            completed: false,
        };
        Response::from_parts(parts, Body::from_stream(stream))
    } else {
        let duration = scope.complete(status.as_u16(), &error_type);
        context.emit(duration, &error_type, &cause);
        response
    }
}

struct LifecycleBody {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>,
    _active: ActiveRequest,
}

impl Stream for LifecycleBody {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

struct AccessContext {
    request_id: String,
    trace_id: String,
    span_id: String,
    principal: String,
    method: axum::http::Method,
    route: String,
    path: String,
    status: axum::http::StatusCode,
    span: tracing::Span,
}

impl AccessContext {
    fn emit(&self, duration: f64, error_type: &str, cause: &str) {
        let _entered = self.span.enter();
        macro_rules! emit {
            ($level:ident) => {
                tracing::$level!(
                    event.name = "http.access",
                    request_id = %self.request_id,
                    trace_id = %self.trace_id,
                    span_id = %self.span_id,
                    principal = %self.principal,
                    http.request.method = %self.method,
                    http.route = %self.route,
                    url.path = %self.path,
                    http.response.status_code = self.status.as_u16(),
                    http.server.request.duration = duration,
                    error.type = %error_type,
                    exception.message = %cause,
                    "request completed"
                )
            };
        }
        if self.status.is_server_error() || error_type == "stream_body_error" {
            emit!(error);
        } else if self.status.is_client_error() || error_type == "cancelled" {
            emit!(warn);
        } else {
            emit!(info);
        }
    }
}

struct TrackedBody {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>,
    scope: Option<RequestScope>,
    context: AccessContext,
    completed: bool,
}

impl TrackedBody {
    fn finish(&mut self, error_type: &str, cause: &str) {
        let Some(mut scope) = self.scope.take() else {
            return;
        };
        let duration = scope.complete(self.context.status.as_u16(), error_type);
        self.context.emit(duration, error_type, cause);
        self.completed = true;
    }
}

impl Stream for TrackedBody {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(None) => {
                self.finish("", "");
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(error))) => {
                self.finish("stream_body_error", &error.to_string());
                Poll::Ready(Some(Err(error)))
            }
            other => other,
        }
    }
}

impl Drop for TrackedBody {
    fn drop(&mut self) {
        if !self.completed {
            self.finish("cancelled", "the client dropped the response body");
        }
    }
}
