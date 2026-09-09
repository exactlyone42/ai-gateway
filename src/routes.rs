mod chat_completions;
mod health;

use std::sync::Arc;

use axum::{
    Router,
    http::HeaderName,
    routing::{get, post},
};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::state::AppState;

/// The route table: one line per endpoint, plus the layers every request crosses.
pub fn router(state: Arc<AppState>) -> Router {
    let request_id_header = HeaderName::from_static("x-request-id");

    Router::new()
        .route("/healthz", get(health::handle))
        .route("/v1/chat/completions", post(chat_completions::handle))
        .with_state(state)
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
}
