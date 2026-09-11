use std::sync::Arc;

use axum::{
    Extension, Json,
    body::{Body, Bytes},
    extract::{OriginalUri, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use futures_util::StreamExt as _;
use serde_json::{Value, json};

use crate::{
    adapters::{
        ProviderRequestInput, prepare_for_provider, protocol, provider, response,
        stream::{CrossStreamEncoder, CrossStreamObserver},
    },
    canonical::{PrincipalRef, RequestEnrichment},
    error::GatewayError,
    identity::PrincipalContext,
    ledger::{ProviderAttempt, RequestCompletion, RequestStart, TokenUsage},
    logging,
    middleware::{REQUEST_ID_HEADER, RequestReceivedAt},
    model_registry::{CapabilityRequirements, GatewayOperation, ModelSelector},
    policy::value_text,
    routes::{BodyBytes, body_bytes},
    services::{
        request_policy, request_policy::RejectionContext, request_quota, sse::MetadataParser,
        streaming,
    },
    state::AppState,
};

/// Tells the access layer that the request ends with the body, not headers.
#[derive(Clone, Copy)]
pub(crate) struct StreamingResponse;

#[derive(Clone, Copy)]
pub(super) enum Operation {
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

pub(super) async fn proxy(
    state: Arc<AppState>,
    principal: PrincipalContext,
    received_at: DateTime<Utc>,
    path_and_query: &str,
    headers: HeaderMap,
    body: Bytes,
    operation: Operation,
) -> Result<Response, GatewayError> {
    let gateway_operation = operation.gateway();
    let body_len = body.len();
    let decoded =
        protocol::decode(gateway_operation, body).map_err(GatewayError::InvalidRequest)?;
    let selector = ModelSelector::parse(decoded.requested_model())?;
    let session_id = header_text(&headers, "x-claude-code-session-id")
        .or_else(|| header_text(&headers, "x-session-id"));
    let prepared = request_policy::prepare(
        &state,
        &headers,
        selector,
        RejectionContext {
            principal: principal.clone(),
            received_at,
            request_id: ledger_request_id(&headers),
            session_id: session_id.clone(),
            operation: operation.name(),
            client_agent_id: header_text(&headers, "x-claude-code-agent-id"),
            parent_agent_id: header_text(&headers, "x-claude-code-parent-agent-id"),
            estimated_tokens: matches!(operation, Operation::Messages)
                .then(|| i64::try_from(estimate_input_tokens(body_len)).unwrap_or(i64::MAX)),
        },
    )
    .await?;
    let policy = &prepared.policy;
    let required = CapabilityRequirements::from_request(gateway_operation, decoded.document());
    let gateway_request_id = uuid::Uuid::new_v4();
    let trace_id = match logging::current_trace_ids().0 {
        value if value.is_empty() => None,
        value => Some(value),
    };
    let canonical = decoded.enrich(RequestEnrichment {
        gateway_request_id,
        trace_id: trace_id.clone(),
        session_id: session_id.clone(),
        principal: PrincipalRef::from(&principal),
        requested_model: prepared.requested_model.clone(),
        requested_quality: None,
        workload_class: value_text(&policy.workload_class),
        urgency: value_text(&policy.urgency),
        budget_class: value_text(&policy.budget_class),
        deadline: policy.deadline_at,
    });
    let prepared_quota = request_quota::admit(
        &state,
        &prepared,
        gateway_request_id,
        &canonical,
        body_len,
        required,
    )
    .await?;
    let routing_reason = prepared_quota.routing_reason;
    let routing_claim = prepared_quota.routing_claim;
    let translation = prepared_quota.translation;
    let crate::quota::QuotaAdmission {
        selection,
        estimated_usage: estimated,
        snapshot: quota_snapshot,
        concurrency,
    } = prepared_quota.admission;
    let model = selection.public_model_id.clone();
    let provider_model = selection.provider_model_id.clone();
    let provider = selection.provider_key.clone();
    let profile = selection.profile.clone();
    let selection_snapshot = selection.ledger_snapshot();
    let ledger_estimated = matches!(operation, Operation::Messages).then_some(estimated.clone());
    let upstream_request = prepare_for_provider(
        &canonical,
        &profile,
        &provider_model,
        translation,
        ProviderRequestInput {
            source_path_and_query: path_and_query,
            client_headers: &headers,
            idempotency_key: None,
        },
    )?;
    let is_stream = matches!(operation, Operation::Messages) && canonical.stream;
    let mut reservation = state
        .ledger
        .reserve_with_quota(
            RequestStart {
                request_id: ledger_request_id(&headers),
                session_id: canonical.session_id.clone(),
                trace_id: canonical.trace_id.clone(),
                requested_model: Some(canonical.requested_model.clone()),
                workload_class: Some(canonical.workload_class.clone()),
                urgency: Some(canonical.urgency.clone()),
                quality: Some(value_text(&policy.quality)),
                budget_class: Some(canonical.budget_class.clone()),
                estimated_tokens: ledger_estimated
                    .as_ref()
                    .map(request_quota::estimated_tokens),
                selection: selection_snapshot.clone(),
                policy: Some(policy.ledger_snapshot().with_routing(routing_reason)),
                principal: principal.clone(),
                created_at: received_at,
            },
            Some(gateway_request_id),
            quota_snapshot,
        )
        .await?;
    reservation.annotate_protocol(
        operation.name(),
        header_text(&headers, "x-claude-code-agent-id"),
        header_text(&headers, "x-claude-code-parent-agent-id"),
    );
    let mut reservation = request_policy::enforce_deadline(reservation, policy).await?;
    reservation = crate::services::admission::acquire(
        &state,
        reservation,
        crate::services::admission::AdmissionContext {
            principal: &principal,
            policy,
            selection: &selection,
            quota: concurrency,
            body: canonical.source_document(),
            received_at,
        },
    )
    .await?;
    let provider_request_at = Utc::now();
    reservation.attach_routing_lease(state.routing.begin(
        &provider,
        provider_request_at,
        routing_claim,
    )?);
    let upstream = match provider::send(
        &state.client,
        &profile.base_url,
        upstream_request,
        &profile.provider_key,
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

    let status = upstream.status();
    let response_headers = upstream.headers().clone();
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
        ledger_estimated.unwrap_or_default(),
    )
    .with_selection(selection_snapshot);
    let content_is_sse = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));

    if status.is_success() && is_stream && translation.translated && !content_is_sse {
        let completed_at = Utc::now();
        let mut attempt = attempt;
        attempt.conclude(
            "failed",
            Some("upstream_protocol_error".to_owned()),
            completed_at,
        );
        reservation
            .finish(completion(
                StatusCode::BAD_GATEWAY,
                model,
                provider,
                attempt,
                completed_at,
            ))
            .await?;
        return Err(GatewayError::UpstreamProtocol);
    }

    if !status.is_success() || !is_stream {
        let mut attempt = attempt;
        let bytes = match collect_body(
            upstream,
            &mut attempt,
            if translation.translated {
                state.config.stream_event_max_bytes.saturating_mul(8)
            } else {
                usize::MAX
            },
        )
        .await
        {
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
                return Err(error);
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
            let usage = match provider::usage_for(profile.kind, &bytes) {
                Ok(usage) => usage.map(TokenUsage::from).unwrap_or_default(),
                Err(error) => {
                    attempt.conclude("failed", Some(error.kind().to_owned()), completed_at);
                    reservation.set_client_first_byte_at(completed_at);
                    reservation
                        .finish(completion(
                            StatusCode::BAD_GATEWAY,
                            model,
                            provider,
                            attempt,
                            completed_at,
                        ))
                        .await?;
                    return Err(error);
                }
            };
            attempt.observe_usage(usage, completed_at);
        }
        let client_response = if translation.translated {
            if !status.is_success() {
                None
            } else {
                match response::transcode_buffered_with_outcome(
                    translation.target_operation,
                    translation.source_operation,
                    &bytes,
                    &format!("msg_apg_{}", gateway_request_id.simple()),
                    &model,
                    &canonical,
                ) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        attempt.conclude("failed", Some(error.kind().to_owned()), completed_at);
                        reservation.set_client_first_byte_at(completed_at);
                        reservation
                            .finish(completion(
                                StatusCode::BAD_GATEWAY,
                                model,
                                provider,
                                attempt,
                                completed_at,
                            ))
                            .await?;
                        return Err(error);
                    }
                }
            }
        } else {
            Some(response::TranscodedBufferedResponse {
                body: bytes.clone(),
                incomplete: false,
            })
        };
        let incomplete = client_response
            .as_ref()
            .is_some_and(|response| response.incomplete);
        attempt.conclude(
            if incomplete {
                "failed"
            } else if status.is_success() {
                "completed"
            } else {
                "rejected"
            },
            if incomplete {
                Some("provider_response_incomplete".to_owned())
            } else {
                (!status.is_success()).then(|| status.as_u16().to_string())
            },
            completed_at,
        );
        reservation.set_client_first_byte_at(completed_at);
        let mut request_completion =
            completion(status, model, provider.clone(), attempt, completed_at);
        if incomplete {
            request_completion.status = "failed".to_owned();
        }
        reservation.finish(request_completion).await?;
        if translation.translated && client_response.is_none() {
            return Err(GatewayError::UpstreamProtocol);
        }
        let client_bytes = client_response.ok_or(GatewayError::UpstreamProtocol)?.body;
        let response_headers = if translation.translated {
            translated_response_headers()
        } else {
            response_headers
        };
        return build_response(status, &response_headers, Body::from(client_bytes));
    }

    let body = if translation.translated {
        streaming::relay_transcoded(
            upstream,
            reservation,
            attempt,
            model.clone(),
            provider,
            (
                CrossStreamObserver::new(translation.target_operation)
                    .map_err(|_| GatewayError::InvalidConfiguration)?,
                CrossStreamEncoder::new(
                    translation.source_operation,
                    format!("msg_apg_{}", gateway_request_id.simple()),
                    model,
                    &canonical,
                )
                .map_err(|_| GatewayError::InvalidConfiguration)?,
            ),
            state.config.stream_event_max_bytes,
        )
        .await?
    } else {
        streaming::relay(
            upstream,
            reservation,
            attempt,
            model,
            provider,
            MetadataParser::default(),
            state.config.stream_event_max_bytes,
        )
        .await?
    };
    let response_headers = if translation.translated {
        translated_stream_headers()
    } else {
        response_headers
    };
    let mut response = build_response(status, &response_headers, body)?;
    response.extensions_mut().insert(StreamingResponse);
    Ok(response)
}

fn translated_response_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers
}

fn translated_stream_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers
}

async fn collect_body(
    response: reqwest::Response,
    attempt: &mut ProviderAttempt,
    response_max_bytes: usize,
) -> Result<Bytes, GatewayError> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    let mut first = true;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(GatewayError::Upstream)?;
        if first {
            first = false;
            attempt.observe_first_byte(Utc::now());
        }
        body.extend_from_slice(&chunk);
        if body.len() > response_max_bytes {
            return Err(GatewayError::UpstreamProtocol);
        }
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
