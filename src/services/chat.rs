use axum::{
    body::Bytes,
    http::{HeaderValue, StatusCode, header},
};
use serde_json::Value;

use crate::{error::GatewayError, logging, state::AppState};

/// What the upstream answered, before it is shaped into an HTTP response.
pub struct UpstreamReply {
    pub status: StatusCode,
    pub content_type: Option<HeaderValue>,
    pub body: Bytes,
}

/// Applies model policy, then forwards the completion request upstream.
pub async fn complete(state: &AppState, body: &Value) -> Result<UpstreamReply, GatewayError> {
    ensure_model_allowed(state, body)?;
    forward(state, body).await
}

/// `skip_all` keeps the request body -- prompts included -- out of the span.
#[tracing::instrument(name = "provider.attempt", skip_all)]
async fn forward(state: &AppState, body: &Value) -> Result<UpstreamReply, GatewayError> {
    // The upstream call carries this request's trace, so a slow provider is
    // visible as part of the same trace rather than as a gap.
    let mut headers = axum::http::HeaderMap::new();
    logging::inject_context(&mut headers);

    let response = state
        .client
        .post(format!(
            "{}/v1/chat/completions",
            state.config.litellm_base_url
        ))
        .headers(headers)
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

#[tracing::instrument(name = "policy.evaluate", skip_all)]
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
