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

use crate::{middleware::require_client_key, state::AppState};

/// The route table: which endpoints exist, which of them need a client key,
/// and the layers every request crosses.
pub fn router(state: Arc<AppState>) -> Router {
    let request_id_header = HeaderName::from_static("x-request-id");

    let authenticated = Router::new()
        .route("/v1/chat/completions", post(chat_completions::handle))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_client_key,
        ));

    Router::new()
        .route("/healthz", get(health::handle))
        .merge(authenticated)
        .with_state(state)
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
}
