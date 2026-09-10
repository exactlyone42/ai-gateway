use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, header},
    response::Response,
};
use chrono::Utc;
use serde_json::Value;

use crate::{
    error::GatewayError,
    identity::PrincipalContext,
    ledger::{CompletedAttempt, ProviderAttempt, RequestCompletion, RequestStart},
    logging,
    middleware::REQUEST_ID_HEADER,
    services::chat,
    state::AppState,
};

/// `POST /v1/chat/completions` -- calls the service, shapes its reply into an
/// HTTP response. Authorization already happened in the middleware layer.
pub async fn handle(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<PrincipalContext>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, GatewayError> {
    // W05 owns the true SSE path. Buffering an SSE body here would return data
    // while silently dropping its terminal usage event from the ledger.
    if body.get("stream").and_then(Value::as_bool) == Some(true) {
        return Err(GatewayError::StreamingNotImplemented);
    }
    let requested_model = body
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let request_id = ledger_request_id(&headers);
    let session_id = header_text(&headers, "x-session-id")
        .or_else(|| header_text(&headers, "x-claude-code-session-id"));
    let trace_id = match logging::current_trace_ids().0 {
        value if value.is_empty() => None,
        value => Some(value),
    };
    let started_at = Utc::now();
    let reservation = state
        .ledger
        .reserve(RequestStart {
            request_id,
            session_id,
            trace_id,
            requested_model: requested_model.clone(),
            workload_class: None,
            urgency: None,
            quality: None,
            budget_class: None,
            estimated_tokens: None,
            principal,
            created_at: started_at,
        })
        .await?;
    let upstream_started_at = Utc::now();

    let reply = match chat::complete(&state, &body).await {
        Ok(reply) => reply,
        Err(error) => {
            let completed_at = Utc::now();
            let attempted = matches!(error, GatewayError::Upstream(_));
            let attempts = if attempted {
                vec![ProviderAttempt::failed(
                    state.config.upstream_provider.clone(),
                    requested_model.clone().unwrap_or_default(),
                    upstream_started_at,
                    completed_at,
                    error.kind().to_owned(),
                )]
            } else {
                Vec::new()
            };
            reservation
                .finish(RequestCompletion {
                    status: "failed".to_owned(),
                    selected_model: attempted.then(|| requested_model.clone()).flatten(),
                    selected_provider: attempted.then(|| state.config.upstream_provider.clone()),
                    attempts,
                    completed_at,
                })
                .await?;
            return Err(error);
        }
    };

    let mut builder = Response::builder().status(reply.status);
    if let Some(content_type) = reply.content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    let response = builder.body(axum::body::Body::from(reply.body.clone()))?;
    let completed_at = Utc::now();
    let attempt_status = if reply.status.is_success() {
        "completed"
    } else {
        "rejected"
    };
    let error_type = (!reply.status.is_success()).then(|| reply.status.as_u16().to_string());
    let attempt = ProviderAttempt::completed(
        CompletedAttempt {
            provider: state.config.upstream_provider.clone(),
            model: requested_model.clone().unwrap_or_default(),
            provider_request_id: reply.provider_request_id,
            provider_response_id: reply.provider_response_id,
            started_at: upstream_started_at,
            completed_at,
            status: attempt_status.to_owned(),
            error_type,
        },
        &reply.body,
    );
    reservation
        .finish(RequestCompletion {
            status: if reply.status.is_success() {
                "completed".to_owned()
            } else {
                "failed".to_owned()
            },
            selected_model: requested_model,
            selected_provider: Some(state.config.upstream_provider.clone()),
            attempts: vec![attempt],
            completed_at,
        })
        .await?;
    Ok(response)
}

fn header_text(headers: &HeaderMap, name: &'static str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn ledger_request_id(headers: &HeaderMap) -> String {
    header_text(headers, REQUEST_ID_HEADER)
        .map(|value| value.trim().chars().take(512).collect::<String>())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn invalid_or_effectively_empty_request_ids_get_a_ledger_safe_fallback() {
        let mut headers = HeaderMap::new();
        headers.insert(REQUEST_ID_HEADER, HeaderValue::from_bytes(b"\xff").unwrap());
        assert!(!ledger_request_id(&headers).is_empty());

        headers.insert(
            REQUEST_ID_HEADER,
            HeaderValue::from_str(&format!("{}x", " ".repeat(513))).unwrap(),
        );
        assert_eq!(ledger_request_id(&headers), "x");
    }
}
