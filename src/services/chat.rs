use axum::{
    body::Bytes,
    http::{HeaderValue, StatusCode, header},
};
use serde_json::Value;

use crate::{error::GatewayError, state::AppState};

/// What the upstream answered, before it is shaped into an HTTP response.
pub struct UpstreamReply {
    pub status: StatusCode,
    pub content_type: Option<HeaderValue>,
    pub body: Bytes,
}

/// Applies model policy, then forwards the completion request upstream.
pub async fn complete(state: &AppState, body: &Value) -> Result<UpstreamReply, GatewayError> {
    ensure_model_allowed(state, body)?;

    let response = state
        .client
        .post(format!(
            "{}/v1/chat/completions",
            state.config.litellm_base_url
        ))
        .bearer_auth(&state.config.litellm_master_key)
        .json(body)
        .send()
        .await?;
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();

    Ok(UpstreamReply {
        status,
        content_type,
        body: response.bytes().await?,
    })
}

fn ensure_model_allowed(state: &AppState, body: &Value) -> Result<(), GatewayError> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if state
        .config
        .allowed_models
        .iter()
        .any(|allowed| allowed == model)
    {
        Ok(())
    } else {
        Err(GatewayError::ModelForbidden)
    }
}
