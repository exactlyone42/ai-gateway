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
    pub provider_request_id: Option<String>,
    pub provider_response_id: Option<String>,
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
    let provider_request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let body = response.bytes().await?;
    let provider_response_id = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        });

    Ok(UpstreamReply {
        status,
        content_type,
        body,
        provider_request_id,
        provider_response_id,
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
