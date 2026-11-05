use std::collections::HashSet;

use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
};
use chrono::{DateTime, Utc};
use futures_util::StreamExt as _;
use hmac::{Hmac, Mac as _};
use serde_json::Value;
use sha2::Sha256;

use crate::{
    adapters::{
        ProviderRequestInput, ValidatedTranslationPlan, prepare_for_provider, protocol, provider,
        response,
        stream::{CrossStreamEncoder, CrossStreamObserver},
    },
    canonical::{CanonicalRequest, CanonicalStreamEvent, PrincipalRef, RequestEnrichment},
    error::GatewayError,
    identity::PrincipalContext,
    ledger::{ProviderAttempt, RequestCompletion, RequestStart, TokenUsage},
    logging,
    middleware::REQUEST_ID_HEADER,
    model_registry::{CapabilityRequirements, GatewayOperation, ModelSelector, ProviderKind},
    policy::value_text,
    quota::QuotaAdmission,
    services::{
        request_policy::{self, RejectionContext},
        request_quota,
        streaming::{ProtocolError, StreamObserver, event_data, relay, relay_transcoded},
        task_decomposition,
    },
    state::AppState,
};

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Operation {
    Responses,
    ChatCompletions,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat_completions",
        }
    }

    fn observer(self, metadata: &Value) -> Result<OpenAiObserver, GatewayError> {
        match self {
            Self::Responses => Ok(OpenAiObserver::Responses(ResponsesObserver::default())),
            Self::ChatCompletions => {
                let expected_choices = match metadata.get("n") {
                    None => 1,
                    Some(value) => value
                        .as_u64()
                        .filter(|value| (1..=1024).contains(value))
                        .and_then(|value| usize::try_from(value).ok())
                        .ok_or(GatewayError::UnsupportedRequest)?,
                };
                Ok(OpenAiObserver::Chat(ChatObserver::new(expected_choices)))
            }
        }
    }

    fn gateway(self) -> GatewayOperation {
        match self {
            Self::Responses => GatewayOperation::OpenAiResponses,
            Self::ChatCompletions => GatewayOperation::OpenAiChatCompletions,
        }
    }
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Body,
    pub streaming: bool,
}

pub struct RequestContext {
    pub principal: PrincipalContext,
    pub received_at: DateTime<Utc>,
    pub path_and_query: String,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub operation: Operation,
}

struct BufferedContext {
    response: reqwest::Response,
    status: StatusCode,
    headers: HeaderMap,
    reservation: crate::ledger::Reservation,
    attempt: ProviderAttempt,
    model: String,
    provider: String,
    provider_kind: ProviderKind,
    provider_secret: Option<String>,
    operation: Operation,
    translation: ValidatedTranslationPlan,
    gateway_request_id: uuid::Uuid,
    response_max_bytes: usize,
    canonical: CanonicalRequest,
    capture: Option<std::sync::Arc<crate::capture_session::AsyncCaptureSession>>,
}

enum DeferredProtocol {
    Native(OpenAiObserver),
    Translated(Box<(CrossStreamObserver, CrossStreamEncoder)>),
}

pub async fn execute(state: &AppState, request: RequestContext) -> Result<Reply, GatewayError> {
    let operation = request.operation.gateway();
    let body_len = request.body.len();
    let session_id = header_text(&request.headers, "x-codex-session-id")
        .or_else(|| header_text(&request.headers, "x-session-id"));
    // Built before the body is read, so a request that never decodes still has
    // an identity and a request id to record itself with.
    let rejection = RejectionContext {
        principal: request.principal.clone(),
        received_at: request.received_at,
        request_id: ledger_request_id(&request.headers),
        session_id: session_id.clone(),
        operation: request.operation.name(),
        client_agent_id: None,
        parent_agent_id: None,
        estimated_tokens: Some(i64::try_from(estimate_input_tokens(body_len)).unwrap_or(i64::MAX)),
        attribution: crate::ledger::RequestAttribution::from_headers(&request.headers),
    };
    let decoded = match protocol::decode(operation, request.body) {
        Ok(decoded) => decoded,
        Err(error) => {
            request_policy::record_unprocessable(state, &rejection, None, "request_undecodable")
                .await;
            return Err(GatewayError::InvalidRequest(error));
        }
    };
    if request.operation == Operation::Responses
        && decoded
            .document()
            .get("background")
            .and_then(Value::as_bool)
            == Some(true)
    {
        request_policy::record_unprocessable(
            state,
            &rejection,
            Some(decoded.requested_model().to_owned()),
            "background_not_supported",
        )
        .await;
        return Err(GatewayError::UnsupportedRequest);
    }
    let selector = match ModelSelector::parse(decoded.requested_model()) {
        Ok(selector) => selector,
        Err(error) => {
            request_policy::record_unprocessable(
                state,
                &rejection,
                Some(decoded.requested_model().to_owned()),
                "model_name_unparsable",
            )
            .await;
            return Err(GatewayError::from(error));
        }
    };
    let requested_parameters = crate::request_parameters::snapshot(operation, decoded.document());
    let prepared = request_policy::prepare(state, &request.headers, selector, rejection).await?;
    let policy = &prepared.policy;
    let idempotency_key = provider_idempotency_key(state, &request.principal, &request.headers)?;
    let gateway_request_id = uuid::Uuid::new_v4();
    let trace_id = match logging::current_trace_ids().0 {
        value if value.is_empty() => None,
        value => Some(value),
    };
    let canonical = decoded.enrich(RequestEnrichment {
        gateway_request_id,
        trace_id: trace_id.clone(),
        session_id: session_id.clone(),
        principal: PrincipalRef::from(&request.principal),
        requested_model: prepared.requested_model.clone(),
        requested_quality: None,
        workload_class: value_text(&policy.workload_class),
        urgency: value_text(&policy.urgency),
        budget_class: value_text(&policy.budget_class),
        deadline: policy.deadline_at,
    });
    let required = CapabilityRequirements::from_canonical(&canonical);
    let prepared_quota = request_quota::admit(
        state,
        &prepared,
        gateway_request_id,
        &canonical,
        body_len,
        required,
    )
    .await?;
    if state.personal_tokens.is_none() {
        task_decomposition::spawn_shadow(
            state.shadow_context(),
            task_decomposition::DecompositionInput::from_request(&canonical),
            gateway_request_id,
        );
    }
    let routing_reason = prepared_quota.routing_reason;
    let routing_claim = prepared_quota.routing_claim;
    let task_routing = prepared_quota.task_routing;
    let translation = prepared_quota.translation;
    let response_max_bytes = if translation.translated {
        state.config.stream_event_max_bytes.saturating_mul(8)
    } else {
        usize::MAX
    };
    let QuotaAdmission {
        selection,
        estimated_usage: estimated,
        snapshot: quota_snapshot,
        concurrency,
    } = prepared_quota.admission;
    let model = selection.public_model_id.clone();
    let provider_model = selection.provider_model_id.clone();
    let provider = selection.provider_key.clone();
    let profile = state
        .profile_for_user(&canonical.principal, &selection.profile)
        .await?;
    let selection_snapshot = selection.ledger_snapshot();
    let upstream_request = prepare_for_provider(
        &canonical,
        &profile,
        &provider_model,
        translation,
        ProviderRequestInput {
            source_path_and_query: &request.path_and_query,
            client_headers: &request.headers,
            idempotency_key,
        },
    )?;
    let provider_parameters = upstream_request.parameter_snapshot()?;
    let provider_secret = profile
        .bearer_secret()
        .or_else(|| profile.api_key_secret())
        .map(str::to_owned)
        .or_else(|| profile.held_token().map(|token| token.to_string()));
    let mut capture = crate::services::capture_request::start(
        state,
        policy,
        &canonical,
        &request.headers,
        upstream_request.body(),
        provider_secret.as_deref(),
    )
    .await?;
    let is_stream = canonical.stream;
    let mut reservation = state
        .ledger
        .reserve_with_quota(
            RequestStart {
                request_id: ledger_request_id(&request.headers),
                session_id: canonical.session_id.clone(),
                trace_id: canonical.trace_id.clone(),
                requested_model: Some(canonical.requested_model.clone()),
                workload_class: Some(canonical.workload_class.clone()),
                urgency: Some(canonical.urgency.clone()),
                quality: Some(value_text(&policy.quality)),
                budget_class: Some(canonical.budget_class.clone()),
                estimated_tokens: Some(request_quota::estimated_tokens(&estimated)),
                selection: selection_snapshot.clone(),
                policy: Some(
                    policy
                        .ledger_snapshot()
                        .with_routing(routing_reason)
                        .with_task_routing(task_routing),
                ),
                principal: request.principal.clone(),
                created_at: request.received_at,
            },
            Some(gateway_request_id),
            quota_snapshot,
            Some(&prepared.rejection.attribution),
        )
        .await?;
    reservation.annotate_parameters(requested_parameters, provider_parameters);
    if let Some(session) = capture.as_ref() {
        reservation.attach_capture(session.clone());
    }
    reservation.cancel_on_force(state.lifecycle.force_token());
    reservation.annotate_protocol(request.operation.name(), None, None);
    if policy.is_expired(Utc::now()) {
        reservation.annotate_policy(policy.rejection_snapshot("deadline_expired"));
        reservation
            .finish(RequestCompletion {
                status: "rejected".to_owned(),
                selected_model: None,
                selected_provider: None,
                attempts: Vec::new(),
                completed_at: Utc::now(),
            })
            .await?;
        return Err(GatewayError::PolicyDenied);
    }
    reservation = crate::services::admission::acquire(
        state,
        reservation,
        crate::services::admission::AdmissionContext {
            principal: &request.principal,
            policy,
            selection: &selection,
            quota: concurrency,
            body: canonical.source_document(),
            received_at: request.received_at,
        },
    )
    .await?;
    let provider_request_at = Utc::now();
    reservation.attach_routing_lease(state.routing.begin(
        &provider,
        provider_request_at,
        routing_claim,
    )?);
    let mut attempt = ProviderAttempt::begin(
        provider.clone(),
        provider_model.clone(),
        None,
        provider_request_at,
        estimated,
    )
    .with_selection(selection_snapshot);
    reservation
        .checkpoint_provider_attempt(model.clone(), provider.clone(), attempt.clone())
        .await?;
    let provider_kind = profile.kind;
    let client = state.client_for_provider(profile.kind).clone();
    let send_base_url = profile.base_url.clone();
    let send_provider = profile.provider_key.clone();
    let send_model = provider_model.clone();
    let mut send = Box::pin(async move {
        let result = provider::send(
            &client,
            &send_base_url,
            upstream_request,
            &send_provider,
            &send_model,
        )
        .await;
        drop(profile);
        result
    });
    let upstream = tokio::select! {
        result = &mut send => Some(result),
        () = tokio::time::sleep(state.config.stream_heartbeat_interval), if is_stream => None,
    };
    if upstream.is_none() {
        let protocol = if translation.translated {
            DeferredProtocol::Translated(Box::new((
                CrossStreamObserver::new(translation.target_operation)
                    .map_err(|_| GatewayError::InvalidConfiguration)?,
                CrossStreamEncoder::new(
                    translation.source_operation,
                    format!("resp_apg_{}", gateway_request_id.simple()),
                    model.clone(),
                    &canonical,
                )
                .map_err(|_| GatewayError::InvalidConfiguration)?,
            )))
        } else {
            DeferredProtocol::Native(request.operation.observer(canonical.source_document())?)
        };
        let heartbeat_interval = state.config.stream_heartbeat_interval;
        let event_max_bytes = state.config.stream_event_max_bytes;
        let deferred_secret = provider_secret.clone();
        let operation = request.operation;
        reservation.set_client_first_byte_at(Utc::now());
        let deferred_capture = reservation.capture();
        let deferred = async_stream::stream! {
            let event = Bytes::from_static(b": keepalive\n\n");
            if let Err(error) = crate::services::streaming::capture_chunk(&deferred_capture, crate::capture_crypto::CaptureLeg::ClientResponse, &event).await {
                yield Err(error);
                return;
            }
            yield Ok::<Bytes, std::io::Error>(event);
            let mut heartbeat = tokio::time::interval(heartbeat_interval);
            heartbeat.tick().await;
            let upstream = loop {
                tokio::select! {
                    result = &mut send => break result,
                    _ = heartbeat.tick() => {
                        let event = Bytes::from_static(b": keepalive\n\n");
                        if let Err(error) = crate::services::streaming::capture_chunk(&deferred_capture, crate::capture_crypto::CaptureLeg::ClientResponse, &event).await {
                            yield Err(error);
                            return;
                        }
                        yield Ok(event);
                    },
                }
            };
            match upstream {
                Ok(response) if response.status().is_success()
                    && response.headers().get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).is_some_and(|value| value.starts_with("text/event-stream")) =>
                {
                    let provider_request_id = response.headers().get("x-request-id").or_else(|| response.headers().get("request-id"))
                        .and_then(|value| value.to_str().ok()).map(ToOwned::to_owned)
                        .filter(|value| !provider::error_contains_secret(value.as_bytes(), &HeaderMap::new(), deferred_secret.as_deref()));
                    attempt.observe_request_id(provider_request_id);
                    let limits = crate::services::streaming::RelayLimits { event_max_bytes, heartbeat_interval, provider_secret: deferred_secret };
                    let relayed = match protocol {
                        DeferredProtocol::Native(observer) => relay(response, reservation, attempt, model, provider, observer, limits).await,
                        DeferredProtocol::Translated(pair) => relay_transcoded(response, reservation, attempt, model, provider, *pair, limits).await,
                    };
                    match relayed {
                        Ok(body) => {
                            let mut body = body.into_data_stream();
                            while let Some(item) = body.next().await {
                                yield item.map_err(std::io::Error::other);
                            }
                        }
                        Err(error) => yield Err(std::io::Error::other(error)),
                    }
                }
                result => {
                    let completed_at = Utc::now();
                    let error_type = match &result {
                        Ok(response) if response.status().is_success() => "upstream_protocol_error".to_owned(),
                        Ok(response) => format!("upstream_status_{}", response.status().as_u16()),
                        Err(error) => error.kind().to_owned(),
                    };
                    let event = match operation {
                        Operation::Responses => Bytes::from_static(b"event: error\ndata: {\"type\":\"error\",\"code\":\"upstream_unavailable\",\"message\":\"upstream failed after streaming began\",\"param\":null,\"sequence_number\":0}\n\n"),
                        Operation::ChatCompletions => Bytes::from_static(b"data: {\"error\":{\"type\":\"api_error\",\"message\":\"upstream failed after streaming began\"}}\n\n"),
                    };
                    if let Err(error) = crate::services::streaming::capture_deferred_error(&deferred_capture, result.ok(), &event).await {
                        yield Err(error);
                        return;
                    }
                    attempt.conclude("failed", Some(error_type), completed_at);
                    if let Err(error) = reservation.finish(RequestCompletion {
                        status: "failed".to_owned(), selected_model: Some(model), selected_provider: Some(provider),
                        attempts: vec![attempt], completed_at,
                    }).await {
                        tracing::error!(error.type = "ledger_failure_not_finalized", exception.message = %error);
                        if deferred_capture.is_some() {
                            yield Err(std::io::Error::other(error));
                            return;
                        }
                    }
                    yield Ok(event);
                    if let Err(error) = crate::services::streaming::finish_capture(&deferred_capture).await {
                        yield Err(error);
                    }
                }
            }
        };
        return Ok(Reply {
            status: StatusCode::OK,
            headers: translated_stream_headers(),
            body: Body::from_stream(deferred),
            streaming: true,
        });
    }
    let upstream = match upstream.ok_or(GatewayError::InvalidConfiguration)? {
        Ok(response) => response,
        Err(error) => {
            crate::services::capture_request::save_error(&capture, &[], &error).await?;
            let completed_at = Utc::now();
            attempt.conclude("failed", Some(error.kind().to_owned()), completed_at);
            reservation
                .finish(RequestCompletion {
                    status: "failed".to_owned(),
                    selected_model: Some(model.clone()),
                    selected_provider: Some(provider.clone()),
                    attempts: vec![attempt],
                    completed_at,
                })
                .await?;
            crate::services::capture_request::finish_session(capture.take()).await?;
            return Err(error);
        }
    };
    let status = upstream.status();
    let mut response_headers = response_headers(upstream.headers());
    provider::remove_secret_headers(&mut response_headers, provider_secret.as_deref());
    let provider_request_id = upstream
        .headers()
        .get("x-request-id")
        .or_else(|| upstream.headers().get("request-id"))
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
        .filter(|value| {
            !provider::error_contains_secret(
                value.as_bytes(),
                &HeaderMap::new(),
                provider_secret.as_deref(),
            )
        });
    attempt.observe_request_id(provider_request_id);
    let content_is_sse = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));

    if status.is_success() && is_stream && translation.translated && !content_is_sse {
        let error_body = serde_json::to_vec(&serde_json::json!({"error":{"code":GatewayError::UpstreamProtocol.kind(),"message":GatewayError::UpstreamProtocol.to_string()}}))
            .map_err(|_| GatewayError::CaptureUnavailable)?;
        crate::services::streaming::capture_deferred_error(
            &capture,
            Some(upstream),
            &Bytes::from(error_body),
        )
        .await
        .map_err(|_| GatewayError::CaptureUnavailable)?;
        let completed_at = Utc::now();
        let mut attempt = attempt;
        attempt.conclude(
            "failed",
            Some("upstream_protocol_error".to_owned()),
            completed_at,
        );
        reservation
            .finish(RequestCompletion {
                status: "failed".to_owned(),
                selected_model: Some(model),
                selected_provider: Some(provider),
                attempts: vec![attempt],
                completed_at,
            })
            .await?;
        crate::services::capture_request::finish_session(capture.take()).await?;
        return Err(GatewayError::UpstreamProtocol);
    }

    if status.is_success() && is_stream && content_is_sse {
        let body = if translation.translated {
            relay_transcoded(
                upstream,
                reservation,
                attempt,
                model.clone(),
                provider.clone(),
                (
                    CrossStreamObserver::new(translation.target_operation)
                        .map_err(|_| GatewayError::InvalidConfiguration)?,
                    CrossStreamEncoder::new(
                        translation.source_operation,
                        format!("resp_apg_{}", gateway_request_id.simple()),
                        model,
                        &canonical,
                    )
                    .map_err(|_| GatewayError::InvalidConfiguration)?,
                ),
                crate::services::streaming::RelayLimits {
                    event_max_bytes: state.config.stream_event_max_bytes,
                    heartbeat_interval: state.config.stream_heartbeat_interval,
                    provider_secret: provider_secret.clone(),
                },
            )
            .await?
        } else {
            relay(
                upstream,
                reservation,
                attempt,
                model,
                provider.clone(),
                request.operation.observer(canonical.source_document())?,
                crate::services::streaming::RelayLimits {
                    event_max_bytes: state.config.stream_event_max_bytes,
                    heartbeat_interval: state.config.stream_heartbeat_interval,
                    provider_secret: provider_secret.clone(),
                },
            )
            .await?
        };
        return Ok(Reply {
            status,
            headers: if translation.translated {
                translated_stream_headers()
            } else {
                response_headers
            },
            body,
            streaming: true,
        });
    }

    finish_buffered(BufferedContext {
        response: upstream,
        status,
        headers: response_headers,
        reservation,
        attempt,
        model,
        provider,
        provider_kind,
        provider_secret,
        operation: request.operation,
        translation,
        gateway_request_id,
        response_max_bytes,
        canonical,
        capture,
    })
    .await
}

async fn finish_buffered(context: BufferedContext) -> Result<Reply, GatewayError> {
    let BufferedContext {
        response,
        status,
        headers,
        mut reservation,
        mut attempt,
        model,
        provider,
        provider_kind,
        provider_secret,
        operation,
        translation,
        gateway_request_id,
        response_max_bytes,
        canonical,
        mut capture,
    } = context;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let mut first = true;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let error = GatewayError::Upstream(error);
                crate::services::capture_request::finish_incomplete_error(
                    capture.take(),
                    &bytes,
                    &error,
                )
                .await?;
                let completed_at = Utc::now();
                attempt.conclude(
                    "failed",
                    Some("upstream_body_error".to_owned()),
                    completed_at,
                );
                reservation
                    .finish(RequestCompletion {
                        status: "failed".to_owned(),
                        selected_model: Some(model),
                        selected_provider: Some(provider),
                        attempts: vec![attempt],
                        completed_at,
                    })
                    .await?;
                return Err(error);
            }
        };
        if first {
            first = false;
            attempt.observe_first_byte(Utc::now());
        }
        bytes.extend_from_slice(&chunk);
        if bytes.len() > response_max_bytes {
            crate::services::capture_request::finish_incomplete_error(
                capture.take(),
                &bytes,
                &GatewayError::UpstreamProtocol,
            )
            .await?;
            let completed_at = Utc::now();
            attempt.conclude(
                "failed",
                Some("response_too_large".to_owned()),
                completed_at,
            );
            reservation
                .finish(RequestCompletion {
                    status: "failed".to_owned(),
                    selected_model: Some(model),
                    selected_provider: Some(provider),
                    attempts: vec![attempt],
                    completed_at,
                })
                .await?;
            return Err(GatewayError::UpstreamProtocol);
        }
    }
    let body = Bytes::from(bytes);
    let completed_at = Utc::now();
    let response_value = serde_json::from_slice::<Value>(&body).ok();
    attempt.observe_response_id(
        response_value
            .as_ref()
            .and_then(|value| {
                value
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .filter(|value| {
                !provider::error_contains_secret(
                    value.as_bytes(),
                    &HeaderMap::new(),
                    provider_secret.as_deref(),
                )
            }),
    );
    if status.is_success() {
        let usage = match provider::usage_for(provider_kind, &body) {
            Ok(usage) => usage.map(TokenUsage::from).unwrap_or_default(),
            Err(error) => {
                crate::services::capture_request::save_error(&capture, &body, &error).await?;
                attempt.conclude("failed", Some(error.kind().to_owned()), completed_at);
                reservation.set_client_first_byte_at(completed_at);
                reservation
                    .finish(RequestCompletion {
                        status: "failed".to_owned(),
                        selected_model: Some(model),
                        selected_provider: Some(provider),
                        attempts: vec![attempt],
                        completed_at,
                    })
                    .await?;
                crate::services::capture_request::finish_session(capture.take()).await?;
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
                &body,
                &format!("resp_apg_{}", gateway_request_id.simple()),
                &model,
                &canonical,
            ) {
                Ok(value) => Some(value),
                Err(error) => {
                    crate::services::capture_request::save_error(&capture, &body, &error).await?;
                    attempt.conclude("failed", Some(error.kind().to_owned()), completed_at);
                    reservation.set_client_first_byte_at(completed_at);
                    reservation
                        .finish(RequestCompletion {
                            status: "failed".to_owned(),
                            selected_model: Some(model),
                            selected_provider: Some(provider),
                            attempts: vec![attempt],
                            completed_at,
                        })
                        .await?;
                    crate::services::capture_request::finish_session(capture.take()).await?;
                    return Err(error);
                }
            }
        }
    } else {
        Some(response::TranscodedBufferedResponse {
            body: body.clone(),
            incomplete: false,
        })
    };
    let incomplete = client_response
        .as_ref()
        .is_some_and(|response| response.incomplete);
    let contains_secret = client_response.as_ref().is_some_and(|response| {
        provider::error_contains_secret(&response.body, &headers, provider_secret.as_deref())
    });
    if (contains_secret || client_response.is_none()) && capture.is_some() {
        crate::services::capture_request::save_error(
            &capture,
            &body,
            &GatewayError::UpstreamProtocol,
        )
        .await?;
    }
    if !contains_secret && let Some(response) = client_response.as_ref() {
        crate::services::capture_request::save_buffered(&capture, &body, &response.body).await?;
    }
    let (request_status, attempt_status, error_type) = if contains_secret {
        (
            "failed",
            "failed",
            Some("provider_secret_in_response".to_owned()),
        )
    } else if incomplete {
        (
            "failed",
            "failed",
            Some("provider_response_incomplete".to_owned()),
        )
    } else if translation.translated && status.is_success() {
        ("completed", "completed", None)
    } else {
        buffered_outcome(operation, status, response_value.as_ref())
    };
    attempt.conclude(attempt_status, error_type, completed_at);
    reservation.set_client_first_byte_at(completed_at);
    reservation
        .finish(RequestCompletion {
            status: request_status.to_owned(),
            selected_model: Some(model),
            selected_provider: Some(provider),
            attempts: vec![attempt],
            completed_at,
        })
        .await?;
    crate::services::capture_request::finish_session(capture.take()).await?;
    if translation.translated && client_response.is_none() {
        return Err(GatewayError::UpstreamProtocol);
    }
    let client_body = client_response.ok_or(GatewayError::UpstreamProtocol)?.body;
    if contains_secret {
        return Err(GatewayError::UpstreamProtocol);
    }
    Ok(Reply {
        status,
        headers: if translation.translated {
            translated_response_headers()
        } else {
            headers
        },
        body: Body::from(client_body),
        streaming: false,
    })
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

fn buffered_outcome(
    operation: Operation,
    status: StatusCode,
    response: Option<&Value>,
) -> (&'static str, &'static str, Option<String>) {
    if !status.is_success() {
        return ("failed", "rejected", Some(status.as_u16().to_string()));
    }
    if operation == Operation::ChatCompletions {
        return ("completed", "completed", None);
    }
    match response
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
    {
        Some("completed") => ("completed", "completed", None),
        Some("failed") => (
            "failed",
            "failed",
            Some("provider_response_failed".to_owned()),
        ),
        Some("incomplete") => (
            "failed",
            "failed",
            Some("provider_response_incomplete".to_owned()),
        ),
        _ => (
            "failed",
            "failed",
            Some("provider_response_not_terminal".to_owned()),
        ),
    }
}

fn provider_idempotency_key(
    state: &AppState,
    principal: &PrincipalContext,
    headers: &HeaderMap,
) -> Result<Option<HeaderValue>, GatewayError> {
    let Some(client_key) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(&state.config.credential_hash_key)
        .map_err(|_| GatewayError::InvalidConfiguration)?;
    mac.update(principal.tenant_id.as_bytes());
    mac.update(&[0]);
    mac.update(client_key.as_bytes());
    let derived = format!(
        "apg-{}",
        base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            mac.finalize().into_bytes(),
        )
    );
    HeaderValue::from_str(&derived)
        .map(Some)
        .map_err(|_| GatewayError::InvalidConfiguration)
}

#[tracing::instrument(name = "token.estimate", skip_all)]
fn estimate_input_tokens(bytes: usize) -> u64 {
    u64::try_from(bytes.div_ceil(4)).unwrap_or(u64::MAX)
}

fn response_headers(upstream: &HeaderMap) -> HeaderMap {
    let mut outgoing = HeaderMap::new();
    for (name, value) in upstream {
        if response_header_allowed(name) {
            outgoing.append(name.clone(), value.clone());
        }
    }
    outgoing
}

fn response_header_allowed(name: &HeaderName) -> bool {
    name == header::CONTENT_TYPE
        || name == header::CONTENT_ENCODING
        || name == header::RETRY_AFTER
        || name.as_str() == "request-id"
        || name.as_str() == "x-request-id"
        || name.as_str().starts_with("openai-")
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

enum OpenAiObserver {
    Responses(ResponsesObserver),
    Chat(ChatObserver),
}

impl StreamObserver for OpenAiObserver {
    fn observe(&mut self, event: &[u8]) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
        match self {
            Self::Responses(observer) => observer.observe(event),
            Self::Chat(observer) => observer.observe(event),
        }
    }

    fn finish(&mut self) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
        match self {
            Self::Responses(observer) => observer.finish(),
            Self::Chat(observer) => observer.finish(),
        }
    }
}

#[derive(Default)]
struct ResponsesObserver {
    usage: TokenUsage,
    response_id: Option<String>,
}

impl StreamObserver for ResponsesObserver {
    fn observe(&mut self, event: &[u8]) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
        let value = strict_json_event(event)?;
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(ProtocolError::Invalid("Responses SSE event has no type"))?;
        if kind == "[DONE]" {
            return Err(ProtocolError::Invalid(
                "Responses stream ended with [DONE] instead of a terminal response event",
            ));
        }
        let mut events = response_metadata(&value, &mut self.response_id, &mut self.usage)?;
        match kind {
            "response.completed" => events.push(CanonicalStreamEvent::ResponseCompleted),
            "response.failed" => events.push(CanonicalStreamEvent::Error(
                crate::canonical::CanonicalError::provider("provider_response_failed"),
            )),
            "response.incomplete" => events.push(CanonicalStreamEvent::Error(
                crate::canonical::CanonicalError::provider("provider_response_incomplete"),
            )),
            "error" => events.push(CanonicalStreamEvent::Error(
                crate::canonical::CanonicalError::provider("provider_stream_error"),
            )),
            _ => {}
        }
        Ok(events)
    }
}

struct ChatObserver {
    usage: TokenUsage,
    response_id: Option<String>,
    expected_choices: usize,
    finished_choices: HashSet<u64>,
}

impl ChatObserver {
    fn new(expected_choices: usize) -> Self {
        Self {
            usage: TokenUsage::default(),
            response_id: None,
            expected_choices,
            finished_choices: HashSet::new(),
        }
    }

    fn all_finished(&self) -> bool {
        self.finished_choices.len() == self.expected_choices
    }
}

impl StreamObserver for ChatObserver {
    fn observe(&mut self, event: &[u8]) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
        let data = event_data(event);
        if !data.has_data {
            return if data.has_non_comment_field {
                Err(ProtocolError::Invalid("OpenAI SSE event is missing data:"))
            } else {
                Ok(Vec::new())
            };
        }
        if data.bytes == b"[DONE]" {
            return if self.all_finished() {
                Ok(vec![CanonicalStreamEvent::ResponseCompleted])
            } else {
                Err(ProtocolError::Invalid(
                    "Chat stream ended before a finish_reason",
                ))
            };
        }
        let value: Value = serde_json::from_slice(&data.bytes)
            .map_err(|_| ProtocolError::Invalid("OpenAI SSE data is not valid JSON"))?;
        let mut events = Vec::new();
        if let Some(id) = value.get("id").and_then(Value::as_str)
            && self.response_id.as_deref() != Some(id)
        {
            self.response_id = Some(id.to_owned());
            events.push(CanonicalStreamEvent::ResponseStarted(id.to_owned()));
        }
        if let Some(usage) = value.get("usage").filter(|usage| usage.is_object())
            && let Some(usage) = merge_usage(&mut self.usage, usage)?
        {
            events.push(CanonicalStreamEvent::Usage(usage));
        }
        if value.get("error").is_some_and(|error| !error.is_null()) {
            events.push(CanonicalStreamEvent::Error(
                crate::canonical::CanonicalError::provider("provider_stream_error"),
            ));
        } else {
            let was_finished = self.all_finished();
            for choice in value
                .get("choices")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let (Some(reason), Some(index)) = (
                    choice.get("finish_reason").and_then(Value::as_str),
                    choice.get("index").and_then(Value::as_u64),
                ) && self.finished_choices.insert(index)
                {
                    events.push(CanonicalStreamEvent::OutputCompleted {
                        index: u32::try_from(index).map_err(|_| {
                            ProtocolError::Invalid("Chat choice index exceeds canonical range")
                        })?,
                        finish_reason: reason.to_owned(),
                    });
                }
            }
            if !was_finished && self.all_finished() {
                events.push(CanonicalStreamEvent::CompletionHint);
            }
        }
        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
        self.all_finished()
            .then(|| vec![CanonicalStreamEvent::ResponseCompleted])
            .ok_or(ProtocolError::Invalid(
                "Chat stream ended before a finish_reason",
            ))
    }
}

fn strict_json_event(event: &[u8]) -> Result<Option<Value>, ProtocolError> {
    let data = event_data(event);
    if !data.has_data {
        return if data.has_non_comment_field {
            Err(ProtocolError::Invalid("OpenAI SSE event is missing data:"))
        } else {
            Ok(None)
        };
    }
    if data.bytes == b"[DONE]" {
        return Ok(Some(serde_json::json!({"type": "[DONE]"})));
    }
    serde_json::from_slice(&data.bytes)
        .map(Some)
        .map_err(|_| ProtocolError::Invalid("OpenAI SSE data is not valid JSON"))
}

fn response_metadata(
    value: &Value,
    response_id: &mut Option<String>,
    usage: &mut TokenUsage,
) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
    let mut events = Vec::new();
    if let Some(response) = value.get("response") {
        if let Some(id) = response.get("id").and_then(Value::as_str)
            && response_id.as_deref() != Some(id)
        {
            *response_id = Some(id.to_owned());
            events.push(CanonicalStreamEvent::ResponseStarted(id.to_owned()));
        }
        if let Some(next) = response.get("usage").filter(|next| next.is_object())
            && let Some(normalized) = merge_usage(usage, next)?
        {
            events.push(CanonicalStreamEvent::Usage(normalized));
        }
    }
    Ok(events)
}

fn merge_usage(
    current: &mut TokenUsage,
    value: &Value,
) -> Result<Option<crate::canonical::CanonicalUsage>, ProtocolError> {
    let before = current.clone();
    set_if_present(value, "input_tokens", &mut current.input_tokens);
    set_if_present(value, "prompt_tokens", &mut current.input_tokens);
    set_if_present(value, "output_tokens", &mut current.output_tokens);
    set_if_present(value, "completion_tokens", &mut current.output_tokens);
    if let Some(details) = value
        .get("input_tokens_details")
        .or_else(|| value.get("prompt_tokens_details"))
    {
        set_if_present(details, "cached_tokens", &mut current.cached_input_tokens);
    }
    if let Some(details) = value.get("output_tokens_details") {
        set_if_present(details, "reasoning_tokens", &mut current.reasoning_tokens);
    }
    if let Some(details) = value
        .get("input_tokens_details")
        .or_else(|| value.get("prompt_tokens_details"))
    {
        set_if_present(
            details,
            "cache_write_tokens",
            &mut current.cache_write_input_tokens,
        );
    }
    if *current == before {
        return Ok(None);
    }
    provider::normalize_openai_usage(current.clone())
        .map(Some)
        .map_err(|_| ProtocolError::Invalid("OpenAI usage details exceed their totals"))
}

fn set_if_present(value: &Value, key: &str, target: &mut u64) {
    if let Some(next) = value.get(key).and_then(Value::as_u64) {
        *target = next;
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatObserver, ResponsesObserver};
    use crate::{canonical::CanonicalStreamEvent, services::streaming::StreamObserver};

    #[test]
    fn responses_requires_an_explicit_terminal_event() {
        let mut observer = ResponsesObserver::default();
        let events = observer
            .observe(b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n")
            .unwrap();
        assert!(events.contains(&CanonicalStreamEvent::ResponseStarted("resp_1".to_owned())));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CanonicalStreamEvent::ResponseCompleted))
        );
        assert!(ResponsesObserver::default().finish().is_err());
    }

    #[test]
    fn responses_rejects_ndjson_even_when_its_object_claims_completion() {
        let mut observer = ResponsesObserver::default();
        let result = observer
            .observe(b"{\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\"}}\n\n");
        assert!(result.is_err() || observer.finish().is_err());
    }

    #[test]
    fn chat_accepts_done_or_eof_after_finish_reason_and_keeps_late_usage() {
        let mut observer = ChatObserver::new(1);
        let finish = observer
            .observe(b"data: {\"id\":\"chat_1\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\n\n")
            .unwrap();
        assert!(finish.contains(&CanonicalStreamEvent::OutputCompleted {
            index: 0,
            finish_reason: "stop".to_owned(),
        }));
        assert!(finish.contains(&CanonicalStreamEvent::CompletionHint));
        let usage = observer
            .observe(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}\n\n")
            .unwrap();
        assert!(usage.iter().any(|event| matches!(
            event,
            CanonicalStreamEvent::Usage(value)
                if value.input_tokens == 7 && value.output_tokens == 3
        )));
        assert!(
            observer
                .finish()
                .unwrap()
                .iter()
                .any(|event| matches!(event, CanonicalStreamEvent::ResponseCompleted))
        );

        let mut with_done = ChatObserver::new(1);
        with_done
            .observe(b"data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\n\n")
            .unwrap();
        assert!(
            with_done
                .observe(b"data: [DONE]\n\n")
                .unwrap()
                .iter()
                .any(|event| matches!(event, CanonicalStreamEvent::ResponseCompleted))
        );
    }

    #[test]
    fn chat_waits_for_every_requested_choice() {
        let mut observer = ChatObserver::new(2);
        let first = observer
            .observe(b"data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\n\n")
            .unwrap();
        assert_eq!(
            first,
            vec![CanonicalStreamEvent::OutputCompleted {
                index: 0,
                finish_reason: "stop".to_owned(),
            }]
        );
        assert!(observer.finish().is_err());

        let second = observer
            .observe(b"data: {\"choices\":[{\"index\":1,\"finish_reason\":\"length\"}]}\n\n")
            .unwrap();
        assert_eq!(
            second,
            vec![
                CanonicalStreamEvent::OutputCompleted {
                    index: 1,
                    finish_reason: "length".to_owned(),
                },
                CanonicalStreamEvent::CompletionHint,
            ]
        );
        assert_eq!(
            observer.finish().unwrap(),
            vec![CanonicalStreamEvent::ResponseCompleted]
        );
    }

    #[test]
    fn repeated_chat_metadata_does_not_create_redundant_checkpoints() {
        let mut observer = ChatObserver::new(1);
        let event = b"data: {\"id\":\"chat_same\",\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}\n\n";
        let first = observer.observe(event).unwrap();
        assert!(first.iter().any(|item| matches!(
            item,
            CanonicalStreamEvent::ResponseStarted(id) if id == "chat_same"
        )));
        assert!(first.iter().any(|item| matches!(
            item,
            CanonicalStreamEvent::Usage(usage)
                if usage.input_tokens == 7 && usage.output_tokens == 3
        )));
        assert!(observer.observe(event).unwrap().is_empty());
    }

    #[test]
    fn malformed_or_missing_data_is_rejected() {
        let mut observer = ResponsesObserver::default();
        assert!(
            observer
                .observe(b"{\"type\":\"response.completed\"}\n\n")
                .is_err()
        );
        assert!(observer.observe(b"data: {broken\n\n").is_err());
    }
}
