use axum::http::{HeaderMap, header};
use std::sync::Arc;

use crate::{
    canonical::CanonicalRequest,
    capture_crypto::{CaptureIdentity, CaptureLeg},
    capture_session::{AsyncCaptureSession, CapturePaths},
    capture_spool::CaptureLimits,
    error::GatewayError,
    policy::{ContentCaptureMode, EffectivePolicy},
    state::AppState,
};

pub(crate) async fn start(
    state: &AppState,
    policy: &EffectivePolicy,
    request: &CanonicalRequest,
    headers: &HeaderMap,
    provider_body: &[u8],
    provider_secret: Option<&str>,
) -> Result<Option<Arc<AsyncCaptureSession>>, GatewayError> {
    if policy.content_capture != ContentCaptureMode::Full {
        return Ok(None);
    }
    let user = request
        .principal
        .user_id
        .ok_or(GatewayError::CaptureUnavailable)?;
    let identity = CaptureIdentity {
        tenant: request.principal.tenant_id,
        capture: request.gateway_request_id,
        user,
        created_ms: chrono::Utc::now().timestamp_millis(),
        retention_days: policy.content_retention_days,
    };
    let mut secrets = Vec::new();
    if let Some(secret) = provider_secret.filter(|value| !value.is_empty()) {
        secrets.push(secret.as_bytes());
    }
    for name in [
        header::AUTHORIZATION.as_str(),
        "x-api-key",
        "x-goog-api-key",
        header::COOKIE.as_str(),
    ] {
        for value in headers.get_all(name) {
            let value = value.as_bytes();
            let secret = if name == header::AUTHORIZATION.as_str() {
                let mut parts = value.splitn(2, u8::is_ascii_whitespace);
                let scheme = parts.next().unwrap_or_default();
                if scheme.eq_ignore_ascii_case(b"bearer") {
                    parts.next().unwrap_or_default().trim_ascii_start()
                } else {
                    value
                }
            } else {
                value
            };
            if !secret.is_empty() && !secrets.contains(&secret) {
                secrets.push(secret);
            }
        }
    }
    let session = AsyncCaptureSession::create_from_keyring(
        &state.pool,
        CapturePaths {
            spool: &state.config.content_capture_spool_dir,
            anchors: &state.config.content_capture_anchor_dir,
        },
        &state.config.content_capture_key_dir,
        identity,
        &secrets,
        CaptureLimits::default(),
        state.capture_slots.clone(),
    )
    .await
    .map_err(|_| GatewayError::CaptureUnavailable)?;
    session
        .save_requests(request.native().body.to_vec(), provider_body.to_vec())
        .await
        .map_err(|_| GatewayError::CaptureUnavailable)?;
    Ok(Some(Arc::new(session)))
}

#[cfg(test)]
pub(crate) async fn finish_buffered(
    session: Option<Arc<AsyncCaptureSession>>,
    provider_body: &[u8],
    client_body: &[u8],
) -> Result<(), GatewayError> {
    save_buffered(&session, provider_body, client_body).await?;
    finish_session(session).await
}

pub(crate) async fn save_buffered(
    session: &Option<Arc<AsyncCaptureSession>>,
    provider_body: &[u8],
    client_body: &[u8],
) -> Result<(), GatewayError> {
    if let Some(session) = session {
        session
            .save_response(CaptureLeg::ProviderResponse, provider_body.to_vec())
            .await
            .map_err(|_| GatewayError::CaptureUnavailable)?;
        session
            .save_response(CaptureLeg::ClientResponse, client_body.to_vec())
            .await
            .map_err(|_| GatewayError::CaptureUnavailable)?;
    }
    Ok(())
}

pub(crate) async fn finish_session(
    session: Option<Arc<AsyncCaptureSession>>,
) -> Result<(), GatewayError> {
    if let Some(session) = session {
        session
            .finish()
            .await
            .map_err(|_| GatewayError::CaptureUnavailable)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) async fn finish_error(
    session: Option<Arc<AsyncCaptureSession>>,
    provider_body: &[u8],
    error: &GatewayError,
) -> Result<(), GatewayError> {
    save_error(&session, provider_body, error).await?;
    finish_session(session).await
}

pub(crate) async fn save_error(
    session: &Option<Arc<AsyncCaptureSession>>,
    provider_body: &[u8],
    error: &GatewayError,
) -> Result<(), GatewayError> {
    if session.is_none() {
        return Ok(());
    }
    let client_body = serde_json::to_vec(
        &serde_json::json!({"error":{"code":error.kind(),"message":error.to_string()}}),
    )
    .map_err(|_| GatewayError::CaptureUnavailable)?;
    save_buffered(session, provider_body, &client_body).await
}

pub(crate) async fn finish_incomplete_error(
    session: Option<Arc<AsyncCaptureSession>>,
    provider_partial: &[u8],
    error: &GatewayError,
) -> Result<(), GatewayError> {
    if let Some(session) = session {
        let client_body = serde_json::to_vec(
            &serde_json::json!({"error":{"code":error.kind(),"message":error.to_string()}}),
        )
        .map_err(|_| GatewayError::CaptureUnavailable)?;
        session
            .save_response(CaptureLeg::ProviderResponse, provider_partial.to_vec())
            .await
            .map_err(|_| GatewayError::CaptureUnavailable)?;
        session
            .save_response(CaptureLeg::ClientResponse, client_body)
            .await
            .map_err(|_| GatewayError::CaptureUnavailable)?;
    }
    Ok(())
}
