mod admin;
mod chat_completions;
mod health;
mod messages;
mod responses;

pub(crate) use messages::StreamingResponse;

use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, rejection::BytesRejection},
    http::{HeaderName, HeaderValue, header},
    routing::{get, head, post},
};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
};

use crate::middleware::{REQUEST_ID_HEADER, access_log, process_gate, require_client_key};
use crate::prelude::*;

/// The route table: which endpoints exist, which of them need a client key,
/// and the layers every request crosses.
pub fn router(state: Arc<AppState>) -> Router {
    let request_id_header = HeaderName::from_static(REQUEST_ID_HEADER);
    let openai = Router::new()
        .route("/v1/chat/completions", post(chat_completions::handle))
        .route("/v1/responses", post(responses::handle))
        .layer(DefaultBodyLimit::max(state.config.openai_request_max_bytes));
    let anthropic = Router::new()
        .route("/v1/messages", post(messages::messages))
        .route("/v1/messages/count_tokens", post(messages::count_tokens))
        .layer(DefaultBodyLimit::max(
            state.config.anthropic_request_max_bytes,
        ));
    let authenticated = Router::new()
        .merge(openai)
        .merge(anthropic)
        .route("/v1/models", get(messages::models))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_client_key,
        ));
    let admin = admin::router(&state);
    let admin_ui = Router::new()
        .nest_service(
            "/admin",
            ServeDir::new("web/dist")
                .append_index_html_on_directories(true)
                .fallback(ServeFile::new("web/dist/index.html")),
        )
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; font-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
            ),
        ));

    Router::new()
        // `/healthz` is the name the old single endpoint had. Keeping it as
        // an alias means existing callers do not break on this change.
        .route("/healthz", get(health::live))
        .route("/health", get(health::live))
        .route("/ready", get(health::ready))
        .route("/api/hello", head(messages::hello))
        .merge(authenticated)
        .merge(admin)
        .merge(admin_ui)
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            process_gate,
        ))
        // Innermost first: the access log needs the id already set and the
        // status already final, so it sits inside the two id layers and
        // outside the handlers. It replaces `TraceLayer`, which reported the
        // same requests without any of the fields a consumer filters on.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            access_log,
        ))
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
}

pub(crate) type BodyBytes = Result<Bytes, BytesRejection>;

pub(crate) fn body_bytes(body: BodyBytes) -> Result<Bytes, crate::error::GatewayError> {
    body.map_err(|error| {
        if error.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
            crate::error::GatewayError::PayloadTooLarge
        } else {
            crate::error::GatewayError::RequestBodyUnavailable(error.to_string())
        }
    })
}

#[cfg(test)]
mod tests;
