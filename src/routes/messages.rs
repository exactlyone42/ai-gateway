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
    model_registry::{
        CapabilityRequirements, GatewayOperation, ModelSelector, parse_request, rewrite_model,
    },
    policy::value_text,
    routes::{BodyBytes, body_bytes},
    services::{
        anthropic, request_policy, request_policy::RejectionContext, sse::MetadataParser, streaming,
    },
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

    fn gateway(self) -> GatewayOperation {
        match self {
            Self::Messages => GatewayOperation::AnthropicMessages,
            Self::CountTokens => GatewayOperation::AnthropicCountTokens,
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
    let mut metadata = parse_request(&body).map_err(GatewayError::InvalidRequest)?;
    let gateway_operation = operation.gateway();
    let selector = ModelSelector::parse(
        metadata
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    let prepared = request_policy::prepare(
        &state,
        &headers,
        selector,
        RejectionContext {
            principal: principal.clone(),
            received_at,
            request_id: ledger_request_id(&headers),
            session_id: header_text(&headers, "x-claude-code-session-id")
                .or_else(|| header_text(&headers, "x-session-id")),
            operation: operation.name(),
            client_agent_id: header_text(&headers, "x-claude-code-agent-id"),
            parent_agent_id: header_text(&headers, "x-claude-code-parent-agent-id"),
            estimated_tokens: matches!(operation, Operation::Messages)
                .then(|| i64::try_from(estimate_input_tokens(body.len())).unwrap_or(i64::MAX)),
        },
    )
    .await?;
    let requested_model = prepared.requested_model.clone();
    let policy = &prepared.policy;
    let required = CapabilityRequirements::from_request(gateway_operation, &metadata);
    let selection = state
        .models
        .select(
            principal.tenant_id,
            principal.is_development_bootstrap(),
            gateway_operation,
            prepared.selector.clone(),
            required,
        )
        .await?;
    request_policy::enforce_selected_model(
        &state,
        &prepared,
        &selection.public_model_id,
        selection.ledger_snapshot(),
    )
    .await?;
    let model = selection.public_model_id.clone();
    let provider_model = selection.provider_model_id.clone();
    let provider = selection.provider_key.clone();
    let profile = selection.profile.clone();
    let selection_snapshot = selection.ledger_snapshot();
    let body = Bytes::from(rewrite_model(&body, &mut metadata, &provider_model)?);
    let is_stream = matches!(operation, Operation::Messages) && required.streaming;
    let estimated = matches!(operation, Operation::Messages).then(|| TokenUsage {
        input_tokens: estimate_input_tokens(body.len()),
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
            requested_model: Some(requested_model),
            workload_class: Some(value_text(&policy.workload_class)),
            urgency: Some(value_text(&policy.urgency)),
            quality: Some(value_text(&policy.quality)),
            budget_class: Some(value_text(&policy.budget_class)),
            estimated_tokens: estimated
                .as_ref()
                .map(|usage| i64::try_from(usage.input_tokens).unwrap_or(i64::MAX)),
            selection: selection_snapshot.clone(),
            policy: Some(policy.ledger_snapshot()),
            principal,
            created_at: received_at,
        })
        .await?;
    reservation.annotate_protocol(
        operation.name(),
        header_text(&headers, "x-claude-code-agent-id"),
        header_text(&headers, "x-claude-code-parent-agent-id"),
    );
    let mut reservation = request_policy::enforce_deadline(reservation, policy).await?;
    let provider_request_at = Utc::now();
    let upstream = match anthropic::forward(
        &state,
        path_and_query,
        &headers,
        body,
        &profile,
        &provider_model,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let completed_at = Utc::now();
            reservation
                .finish(RequestCompletion {
                    status: "failed".to_owned(),
                    selected_model: Some(model.clone()),
                    selected_provider: Some(provider.clone()),
                    attempts: vec![
                        ProviderAttempt::failed(
                            provider,
                            provider_model,
                            provider_request_at,
                            completed_at,
                            error.kind().to_owned(),
                        )
                        .with_selection(selection_snapshot.clone()),
                    ],
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
        provider.clone(),
        provider_model,
        provider_request_id,
        provider_request_at,
        estimated.unwrap_or_default(),
    )
    .with_selection(selection_snapshot);

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
                        provider.clone(),
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
                provider.clone(),
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
        provider,
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

#[tracing::instrument(name = "token.estimate", skip_all)]
fn estimate_input_tokens(bytes: usize) -> u64 {
    u64::try_from(bytes.div_ceil(4)).unwrap_or(u64::MAX)
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

pub async fn models(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<PrincipalContext>,
) -> Result<Json<Value>, GatewayError> {
    let models = state
        .models
        .list(principal.tenant_id, principal.is_development_bootstrap())
        .await?;
    Ok(Json(json!({
        "object": "list",
        "data": models.into_iter().map(|model| json!({
            "id": model.id,
            "object": "model",
            "created": 0,
            "owned_by": model.provider_key,
            "type": "model",
            "display_name": model.display_name,
            "description": model.description,
            "context_window": model.context_window,
            "max_output_tokens": model.max_output_tokens,
            "capabilities": {
                "streaming": model.supports_streaming,
                "tools": model.supports_tools,
                "reasoning": model.supports_reasoning,
                "images": model.supports_images,
                "structured_output": model.supports_structured_output,
            },
        })).collect::<Vec<_>>()
    })))
}

pub async fn hello() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}
