use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use reqwest::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{config::Config, error::GatewayError};

pub struct AppState {
    config: Config,
    client: Client,
    bootstrap_key_hash: [u8; 32],
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let bootstrap_key_hash = Sha256::digest(config.bootstrap_client_key.as_bytes()).into();
        Self {
            config,
            client: Client::new(),
            bootstrap_key_hash,
        }
    }
}

pub async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "ok"})))
}

pub async fn chat_completions(
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
    Ok(builder
        .body(axum::body::Body::from(bytes))
        .expect("valid response"))
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), GatewayError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(GatewayError::Unauthorized)?;
    let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    if constant_time_equal(&candidate, &state.bootstrap_key_hash) {
        Ok(())
    } else {
        Err(GatewayError::Unauthorized)
    }
}

fn constant_time_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_equal;

    #[test]
    fn constant_time_key_comparison_works() {
        assert!(constant_time_equal(&[1; 32], &[1; 32]));
        assert!(!constant_time_equal(&[1; 32], &[2; 32]));
    }
}
