use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header},
    response::Response,
};
use serde_json::Value;

use crate::{auth::authorize, error::GatewayError, state::AppState};

/// `POST /v1/chat/completions` -- forwards the whole body upstream once the
/// caller is authorized and the requested model is allowed by policy.
pub async fn handle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, GatewayError> {
    authorize(&state, &headers)?;
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !state
        .config
        .allowed_models
        .iter()
        .any(|allowed| allowed == model)
    {
        return Err(GatewayError::ModelForbidden);
    }

    let response = state
        .client
        .post(format!(
            "{}/v1/chat/completions",
            state.config.litellm_base_url
        ))
        .bearer_auth(&state.config.litellm_master_key)
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let bytes = response.bytes().await?;
    let mut builder = Response::builder().status(status);
    if let Some(value) = content_type {
        builder = builder.header(header::CONTENT_TYPE, value);
    }
    Ok(builder.body(axum::body::Body::from(bytes))?)
}
