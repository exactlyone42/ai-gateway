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
    error::GatewayError,
    identity::PrincipalContext,
    ledger::{ProviderAttempt, RequestCompletion, RequestStart, TokenUsage},
    logging,
    middleware::REQUEST_ID_HEADER,
    model_registry::{
        CapabilityRequirements, GatewayOperation, ModelSelector, ProviderProfile, parse_request,
        rewrite_model,
    },
    policy::value_text,
    quota::QuotaAdmission,
    services::{
        request_policy::{self, RejectionContext},
        request_quota,
        streaming::{CanonicalStreamEvent, ProtocolError, StreamObserver, event_data, relay},
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
    operation: Operation,
}

pub async fn execute(state: &AppState, request: RequestContext) -> Result<Reply, GatewayError> {
    let mut metadata = parse_request(&request.body).map_err(GatewayError::InvalidRequest)?;
    if request.operation == Operation::Responses
        && metadata.get("background").and_then(Value::as_bool) == Some(true)
    {
        return Err(GatewayError::UnsupportedRequest);
    }
    let operation = request.operation.gateway();
    let selector = ModelSelector::parse(
        metadata
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    let prepared = request_policy::prepare(
        state,
        &request.headers,
        selector,
        RejectionContext {
            principal: request.principal.clone(),
            received_at: request.received_at,
            request_id: ledger_request_id(&request.headers),
            session_id: header_text(&request.headers, "x-codex-session-id")
                .or_else(|| header_text(&request.headers, "x-session-id")),
            operation: request.operation.name(),
            client_agent_id: None,
            parent_agent_id: None,
            estimated_tokens: Some(
                i64::try_from(estimate_input_tokens(request.body.len())).unwrap_or(i64::MAX),
            ),
        },
    )
    .await?;
    let requested_model = prepared.requested_model.clone();
    let policy = &prepared.policy;
    let required = CapabilityRequirements::from_request(operation, &metadata);
    let observer = request.operation.observer(&metadata)?;
    let idempotency_key = provider_idempotency_key(state, &request.principal, &request.headers)?;
    let prepared_quota = request_quota::admit(
        state,
        &prepared,
        operation,
        &metadata,
        request.body.len(),
        required,
    )
    .await?;
    let gateway_request_id = prepared_quota.gateway_request_id;
    let QuotaAdmission {
        selection,
        estimated_usage: estimated,
        snapshot: quota_snapshot,
        lease: quota_lease,
    } = prepared_quota.admission;
    let model = selection.public_model_id.clone();
    let provider_model = selection.provider_model_id.clone();
    let provider = selection.provider_key.clone();
    let profile = selection.profile.clone();
    let selection_snapshot = selection.ledger_snapshot();
    let body = Bytes::from(rewrite_model(
        &request.body,
        &mut metadata,
        &provider_model,
    )?);
    let is_stream = required.streaming;
    let trace_id = match logging::current_trace_ids().0 {
        value if value.is_empty() => None,
        value => Some(value),
    };
    let mut reservation = state
        .ledger
        .reserve_with_quota(
            RequestStart {
                request_id: ledger_request_id(&request.headers),
                session_id: header_text(&request.headers, "x-codex-session-id")
                    .or_else(|| header_text(&request.headers, "x-session-id")),
                trace_id,
                requested_model: Some(requested_model),
                workload_class: Some(value_text(&policy.workload_class)),
                urgency: Some(value_text(&policy.urgency)),
                quality: Some(value_text(&policy.quality)),
                budget_class: Some(value_text(&policy.budget_class)),
                estimated_tokens: Some(request_quota::estimated_tokens(&estimated)),
                selection: selection_snapshot.clone(),
                policy: Some(policy.ledger_snapshot()),
                principal: request.principal,
                created_at: request.received_at,
            },
            Some(gateway_request_id),
            quota_snapshot,
            quota_lease,
        )
        .await?;
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
    let provider_request_at = Utc::now();
    let upstream = match forward(
        state,
        &request.path_and_query,
        &request.headers,
        body,
        idempotency_key,
        &profile,
        &provider_model,
    )
    .await
    {
        Ok(response) => response,
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
    let response_headers = response_headers(upstream.headers());
    let provider_request_id = upstream
        .headers()
        .get("x-request-id")
        .or_else(|| upstream.headers().get("request-id"))
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let attempt = ProviderAttempt::begin(
        provider.clone(),
        provider_model,
        provider_request_id,
        provider_request_at,
        estimated,
    )
    .with_selection(selection_snapshot);
    let content_is_sse = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));

    if status.is_success() && is_stream && content_is_sse {
        let body = relay(
            upstream,
            reservation,
            attempt,
            model,
            provider.clone(),
            observer,
            state.config.stream_event_max_bytes,
        )
        .await?;
        return Ok(Reply {
            status,
            headers: response_headers,
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
        operation: request.operation,
    })
    .await
}

#[tracing::instrument(
    name = "provider.attempt",
    skip_all,
    fields(gen_ai.system = %profile.provider_key, gen_ai.request.model = %provider_model_id)
)]
async fn forward(
    state: &AppState,
    path_and_query: &str,
    client_headers: &HeaderMap,
    body: Bytes,
    idempotency_key: Option<HeaderValue>,
    profile: &ProviderProfile,
    provider_model_id: &str,
) -> Result<reqwest::Response, GatewayError> {
    let mut headers = provider_headers(client_headers);
    if let Some(value) = idempotency_key {
        headers.insert(HeaderName::from_static("idempotency-key"), value);
    }
    logging::inject_context(&mut headers);
    let authorization = HeaderValue::from_str(&format!(
        "Bearer {}",
        profile
            .bearer_secret()
            .ok_or(GatewayError::InvalidConfiguration)?
    ))
    .map_err(|_| GatewayError::InvalidConfiguration)?;
    headers.insert(header::AUTHORIZATION, authorization);
    Ok(state
        .client
        .post(format!("{}{}", profile.base_url, path_and_query))
        .headers(headers)
        .body(body)
        .send()
        .await?)
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
        operation,
    } = context;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let mut first = true;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
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
                return Err(GatewayError::Upstream(error));
            }
        };
        if first {
            first = false;
            attempt.observe_first_byte(Utc::now());
        }
        bytes.extend_from_slice(&chunk);
    }
    let body = Bytes::from(bytes);
    let completed_at = Utc::now();
    let response_value = serde_json::from_slice::<Value>(&body).ok();
    attempt.observe_response_id(response_value.as_ref().and_then(|value| {
        value
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }));
    if status.is_success() {
        attempt.observe_usage(TokenUsage::from_response(&body), completed_at);
    }
    let (request_status, attempt_status, error_type) =
        buffered_outcome(operation, status, response_value.as_ref());
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
    Ok(Reply {
        status,
        headers,
        body: Body::from(body),
        streaming: false,
    })
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

fn provider_headers(client: &HeaderMap) -> HeaderMap {
    let mut outgoing = HeaderMap::new();
    for (name, value) in client {
        let text = name.as_str();
        let allowed = name == header::CONTENT_TYPE
            || name == header::ACCEPT
            || text == "openai-beta"
            || text == "originator"
            || text == "session_id"
            || text.starts_with("x-stainless-");
        if allowed && name != header::AUTHORIZATION && text != "x-api-key" {
            outgoing.append(name.clone(), value.clone());
        }
    }
    outgoing
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
        let mut events = response_metadata(&value, &mut self.response_id, &mut self.usage);
        match kind {
            "response.completed" => events.push(CanonicalStreamEvent::Completed),
            "response.failed" => {
                events.push(CanonicalStreamEvent::Failed("provider_response_failed"))
            }
            "response.incomplete" => {
                events.push(CanonicalStreamEvent::Failed("provider_response_incomplete"))
            }
            "error" => events.push(CanonicalStreamEvent::Failed("provider_stream_error")),
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
                Ok(vec![CanonicalStreamEvent::Completed])
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
            && merge_usage(&mut self.usage, usage)
        {
            events.push(CanonicalStreamEvent::Usage(self.usage.clone()));
        }
        if value.get("error").is_some_and(|error| !error.is_null()) {
            events.push(CanonicalStreamEvent::Failed("provider_stream_error"));
        } else {
            let was_finished = self.all_finished();
            for choice in value
                .get("choices")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if choice
                    .get("finish_reason")
                    .is_some_and(|reason| !reason.is_null())
                    && let Some(index) = choice.get("index").and_then(Value::as_u64)
                {
                    self.finished_choices.insert(index);
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
            .then(|| vec![CanonicalStreamEvent::Completed])
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
) -> Vec<CanonicalStreamEvent> {
    let mut events = Vec::new();
    if let Some(response) = value.get("response") {
        if let Some(id) = response.get("id").and_then(Value::as_str)
            && response_id.as_deref() != Some(id)
        {
            *response_id = Some(id.to_owned());
            events.push(CanonicalStreamEvent::ResponseStarted(id.to_owned()));
        }
        if let Some(next) = response.get("usage").filter(|next| next.is_object())
            && merge_usage(usage, next)
        {
            events.push(CanonicalStreamEvent::Usage(usage.clone()));
        }
    }
    events
}

fn merge_usage(current: &mut TokenUsage, value: &Value) -> bool {
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
    *current != before
}

fn set_if_present(value: &Value, key: &str, target: &mut u64) {
    if let Some(next) = value.get(key).and_then(Value::as_u64) {
        *target = next;
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatObserver, ResponsesObserver};
    use crate::services::streaming::{CanonicalStreamEvent, StreamObserver};

    #[test]
    fn responses_requires_an_explicit_terminal_event() {
        let mut observer = ResponsesObserver::default();
        let events = observer
            .observe(b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n")
            .unwrap();
        assert!(events.contains(&CanonicalStreamEvent::ResponseStarted("resp_1".to_owned())));
        assert!(events.contains(&CanonicalStreamEvent::Completed));
        assert!(ResponsesObserver::default().finish().is_err());
    }

    #[test]
    fn chat_accepts_done_or_eof_after_finish_reason_and_keeps_late_usage() {
        let mut observer = ChatObserver::new(1);
        let finish = observer
            .observe(b"data: {\"id\":\"chat_1\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\n\n")
            .unwrap();
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
                .contains(&CanonicalStreamEvent::Completed)
        );

        let mut with_done = ChatObserver::new(1);
        with_done
            .observe(b"data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\n\n")
            .unwrap();
        assert!(
            with_done
                .observe(b"data: [DONE]\n\n")
                .unwrap()
                .contains(&CanonicalStreamEvent::Completed)
        );
    }

    #[test]
    fn chat_waits_for_every_requested_choice() {
        let mut observer = ChatObserver::new(2);
        let first = observer
            .observe(b"data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\n\n")
            .unwrap();
        assert!(!first.contains(&CanonicalStreamEvent::CompletionHint));
        assert!(observer.finish().is_err());

        let second = observer
            .observe(b"data: {\"choices\":[{\"index\":1,\"finish_reason\":\"length\"}]}\n\n")
            .unwrap();
        assert!(second.contains(&CanonicalStreamEvent::CompletionHint));
        assert!(observer.finish().is_ok());
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
