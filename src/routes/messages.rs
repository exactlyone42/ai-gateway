use std::sync::Arc;

use axum::{
    Extension, Json,
    body::{Body, Bytes},
    extract::{OriginalUri, State},
    http::{HeaderMap, HeaderName, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use futures_util::StreamExt as _;
use serde_json::{Value, json};

use crate::{
    error::GatewayError,
    identity::PrincipalContext,
    ledger::{ProviderAttempt, RequestCompletion, RequestStart, TokenUsage},
    logging,
    middleware::{REQUEST_ID_HEADER, RequestReceivedAt},
    routes::{BodyBytes, body_bytes},
    services::{anthropic, sse::MetadataParser, streaming},
    state::AppState,
};

/// Tells the access layer that the request ends with the body, not headers.
#[derive(Clone, Copy)]
pub(crate) struct StreamingResponse;

#[derive(Clone, Copy)]
enum Operation {
    Messages,
    CountTokens,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Messages => "messages",
            Self::CountTokens => "count_tokens",
        }
    }
}

pub async fn messages(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<PrincipalContext>,
    Extension(received_at): Extension<RequestReceivedAt>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: BodyBytes,
) -> Result<Response, GatewayError> {
    let body = body_bytes(body)?;
    proxy(
        state,
        principal,
        received_at.0,
        uri_string(&uri),
        headers,
        body,
        Operation::Messages,
    )
    .await
}

pub async fn count_tokens(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<PrincipalContext>,
    Extension(received_at): Extension<RequestReceivedAt>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: BodyBytes,
) -> Result<Response, GatewayError> {
    let body = body_bytes(body)?;
    proxy(
        state,
        principal,
        received_at.0,
        uri_string(&uri),
        headers,
        body,
        Operation::CountTokens,
    )
    .await
}

async fn proxy(
    state: Arc<AppState>,
    principal: PrincipalContext,
    received_at: DateTime<Utc>,
    path_and_query: &str,
    headers: HeaderMap,
    body: Bytes,
    operation: Operation,
) -> Result<Response, GatewayError> {
    let metadata: Value = serde_json::from_slice(&body).map_err(GatewayError::InvalidRequest)?;
    ensure_model_allowed(&state, &metadata)?;
    let model = metadata
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let is_stream = matches!(operation, Operation::Messages)
        && metadata.get("stream").and_then(Value::as_bool) == Some(true);
    let estimated = matches!(operation, Operation::Messages).then(|| TokenUsage {
        input_tokens: u64::try_from(body.len().div_ceil(4)).unwrap_or(u64::MAX),
        ..TokenUsage::default()
    });
    let trace_id = match logging::current_trace_ids().0 {
        value if value.is_empty() => None,
        value => Some(value),
    };
    let mut reservation = state
        .ledger
        .reserve(RequestStart {
            request_id: ledger_request_id(&headers),
            session_id: header_text(&headers, "x-claude-code-session-id")
                .or_else(|| header_text(&headers, "x-session-id")),
            trace_id,
            requested_model: Some(model.clone()),
            workload_class: None,
            urgency: None,
            quality: None,
            budget_class: None,
            estimated_tokens: estimated
                .as_ref()
                .map(|usage| i64::try_from(usage.input_tokens).unwrap_or(i64::MAX)),
            principal,
            created_at: received_at,
        })
        .await?;
    reservation.annotate_protocol(
        operation.name(),
        header_text(&headers, "x-claude-code-agent-id"),
        header_text(&headers, "x-claude-code-parent-agent-id"),
    );

    let provider_request_at = Utc::now();
    let upstream = match anthropic::forward(&state, path_and_query, &headers, body).await {
        Ok(value) => value,
        Err(error) => {
            let completed_at = Utc::now();
            reservation
                .finish(RequestCompletion {
                    status: "failed".to_owned(),
                    selected_model: Some(model.clone()),
                    selected_provider: Some(state.config.anthropic_provider.clone()),
                    attempts: vec![ProviderAttempt::failed(
                        state.config.anthropic_provider.clone(),
                        model,
                        provider_request_at,
                        completed_at,
                        error.kind().to_owned(),
                    )],
                    completed_at,
                })
                .await?;
            return Err(error);
        }
    };

    let status = upstream.response.status();
    let response_headers = upstream.response.headers().clone();
    let provider_request_id = response_headers
        .get("request-id")
        .or_else(|| response_headers.get("x-request-id"))
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let attempt = ProviderAttempt::begin(
        state.config.anthropic_provider.clone(),
        model.clone(),
        provider_request_id,
        provider_request_at,
        estimated.unwrap_or_default(),
    );

    if !status.is_success() || !is_stream {
        let mut attempt = attempt;
        let bytes = match collect_body(upstream.response, &mut attempt).await {
            Ok(bytes) => bytes,
            Err(error) => {
                let failed_at = Utc::now();
                attempt.conclude("failed", Some("upstream_body_error".to_owned()), failed_at);
                reservation
                    .finish(completion(
                        StatusCode::BAD_GATEWAY,
                        model,
                        state.config.anthropic_provider.clone(),
                        attempt,
                        failed_at,
                    ))
                    .await?;
                return Err(GatewayError::Upstream(error));
            }
        };
        let completed_at = Utc::now();
        let response_id = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            });
        attempt.observe_response_id(response_id);
        if status.is_success() && matches!(operation, Operation::Messages) {
            attempt.observe_usage(TokenUsage::from_response(&bytes), completed_at);
        }
        attempt.conclude(
            if status.is_success() {
                "completed"
            } else {
                "rejected"
            },
            (!status.is_success()).then(|| status.as_u16().to_string()),
            completed_at,
        );
        reservation.set_client_first_byte_at(completed_at);
        reservation
            .finish(completion(
                status,
                model,
                state.config.anthropic_provider.clone(),
                attempt,
                completed_at,
            ))
            .await?;
        return build_response(status, &response_headers, Body::from(bytes));
    }

    let body = streaming::relay(
        upstream.response,
        reservation,
        attempt,
        model,
        state.config.anthropic_provider.clone(),
        MetadataParser::default(),
        state.config.stream_event_max_bytes,
    )
    .await?;
    let mut response = build_response(status, &response_headers, body)?;
    response.extensions_mut().insert(StreamingResponse);
    Ok(response)
}

async fn collect_body(
    response: reqwest::Response,
    attempt: &mut ProviderAttempt,
) -> Result<Bytes, reqwest::Error> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    let mut first = true;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if first {
            first = false;
            attempt.observe_first_byte(Utc::now());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

fn completion(
    status: StatusCode,
    model: String,
    provider: String,
    attempt: ProviderAttempt,
    completed_at: DateTime<Utc>,
) -> RequestCompletion {
    RequestCompletion {
        status: if status.is_success() {
            "completed"
        } else {
            "failed"
        }
        .to_owned(),
        selected_model: Some(model),
        selected_provider: Some(provider),
        attempts: vec![attempt],
        completed_at,
    }
}

fn build_response(
    status: StatusCode,
    headers: &HeaderMap,
    body: Body,
) -> Result<Response, GatewayError> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        if response_header_allowed(name) {
            builder = builder.header(name, value);
        }
    }
    Ok(builder.body(body)?)
}

fn response_header_allowed(name: &HeaderName) -> bool {
    name == header::CONTENT_TYPE
        || name == header::CONTENT_ENCODING
        || name == header::RETRY_AFTER
        || name.as_str() == "request-id"
        || name.as_str().starts_with("anthropic-")
}

fn ensure_model_allowed(state: &AppState, body: &Value) -> Result<(), GatewayError> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    state
        .config
        .allowed_models
        .iter()
        .any(|allowed| allowed == model)
        .then_some(())
        .ok_or(GatewayError::ModelForbidden)
}

fn uri_string(uri: &axum::http::Uri) -> &str {
    uri.path_and_query()
        .map(|value| value.as_str())
        .unwrap_or(uri.path())
}

fn header_text(headers: &HeaderMap, name: &'static str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(512).collect())
}

fn ledger_request_id(headers: &HeaderMap) -> String {
    header_text(headers, REQUEST_ID_HEADER)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

pub async fn models(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "object": "list",
        "data": state.config.allowed_models.iter().map(|id| json!({
            "id": id,
            "object": "model",
            "created": 0,
            "owned_by": "gateway",
            "type": "model",
            "display_name": id,
        })).collect::<Vec<_>>()
    }))
}

pub async fn hello() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}
