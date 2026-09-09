use std::sync::Arc;

use axum::{Json, extract::State, http::header, response::Response};
use serde_json::Value;

use crate::{error::GatewayError, services::chat, state::AppState};

/// `POST /v1/chat/completions` -- calls the service, shapes its reply into an
/// HTTP response. Authorization already happened in the middleware layer.
pub async fn handle(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Response, GatewayError> {
    let reply = chat::complete(&state, &body).await?;

    let mut builder = Response::builder().status(reply.status);
    if let Some(content_type) = reply.content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    Ok(builder.body(axum::body::Body::from(reply.body))?)
}
