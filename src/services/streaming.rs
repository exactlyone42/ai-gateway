use std::io;
use std::time::Duration;

use axum::body::{Body, Bytes};
use chrono::Utc;
use futures_util::StreamExt as _;
use thiserror::Error;

use crate::{
    canonical::CanonicalStreamEvent,
    ledger::{ProviderAttempt, RequestCompletion, Reservation},
};

const DEFAULT_EVENT_MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct RelayLimits {
    pub event_max_bytes: usize,
    pub heartbeat_interval: Duration,
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("SSE event exceeded the bounded event size")]
    EventTooLarge,
    #[error("terminal SSE events exceeded the bounded hold size")]
    TerminalTooLarge,
    #[error("provider stream exceeded the bounded total response size")]
    ResponseTooLarge,
    #[error("{0}")]
    Invalid(&'static str),
}

pub trait StreamObserver: Send + 'static {
    fn observe(&mut self, event: &[u8]) -> Result<Vec<CanonicalStreamEvent>, ProtocolError>;

    fn finish(&mut self) -> Result<Vec<CanonicalStreamEvent>, ProtocolError> {
        Err(ProtocolError::Invalid(
            "provider stream ended without a terminal event",
        ))
    }
}

pub trait StreamEncoder: Send + 'static {
    fn encode(&mut self, events: &[CanonicalStreamEvent]) -> Result<Vec<Bytes>, ProtocolError>;
}

struct RelayProtocol<O> {
    observer: O,
    encoder: Option<Box<dyn StreamEncoder>>,
}

pub struct EventDecoder {
    pending: Vec<u8>,
    event_max_bytes: usize,
}

impl Default for EventDecoder {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_MAX_BYTES)
    }
}

impl EventDecoder {
    pub fn new(event_max_bytes: usize) -> Self {
        Self {
            pending: Vec::new(),
            event_max_bytes,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Bytes>, ProtocolError> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = event_end(&self.pending) {
            if end > self.event_max_bytes {
                return Err(ProtocolError::EventTooLarge);
            }
            events.push(Bytes::from(self.pending.drain(..end).collect::<Vec<_>>()));
        }
        if self.pending.len() > self.event_max_bytes {
            return Err(ProtocolError::EventTooLarge);
        }
        Ok(events)
    }

    pub fn finish(&mut self) -> Result<(), ProtocolError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        if self.pending.len() > self.event_max_bytes {
            return Err(ProtocolError::EventTooLarge);
        }
        Err(ProtocolError::Invalid(
            "SSE stream ended with an incomplete event",
        ))
    }
}

pub struct EventData {
    pub bytes: Vec<u8>,
    pub has_data: bool,
    pub has_non_comment_field: bool,
}

pub fn event_data(event: &[u8]) -> EventData {
    let mut data = Vec::new();
    let mut has_data = false;
    let mut has_non_comment_field = false;
    for line in logical_lines(event) {
        if line.is_empty() || line.starts_with(b":") {
            continue;
        }
        if let Some(value) = line.strip_prefix(b"data:") {
            if has_data {
                data.push(b'\n');
            }
            data.extend_from_slice(value.strip_prefix(b" ").unwrap_or(value));
            has_data = true;
        } else {
            has_non_comment_field = true;
        }
    }
    EventData {
        bytes: data,
        has_data,
        has_non_comment_field,
    }
}

pub async fn relay<O: StreamObserver>(
    response: reqwest::Response,
    reservation: Reservation,
    attempt: ProviderAttempt,
    model: String,
    provider: String,
    observer: O,
    limits: RelayLimits,
) -> Result<Body, crate::ledger::LedgerError> {
    relay_inner(
        response,
        reservation,
        attempt,
        model,
        provider,
        RelayProtocol {
            observer,
            encoder: None,
        },
        limits,
    )
    .await
}

pub async fn relay_transcoded<O: StreamObserver, E: StreamEncoder>(
    response: reqwest::Response,
    reservation: Reservation,
    attempt: ProviderAttempt,
    model: String,
    provider: String,
    transcoder: (O, E),
    limits: RelayLimits,
) -> Result<Body, crate::ledger::LedgerError> {
    relay_inner(
        response,
        reservation,
        attempt,
        model,
        provider,
        RelayProtocol {
            observer: transcoder.0,
            encoder: Some(Box::new(transcoder.1)),
        },
        limits,
    )
    .await
}

async fn relay_inner<O: StreamObserver>(
    response: reqwest::Response,
    mut reservation: Reservation,
    attempt: ProviderAttempt,
    model: String,
    provider: String,
    mut protocol: RelayProtocol<O>,
    limits: RelayLimits,
) -> Result<Body, crate::ledger::LedgerError> {
    // The stream owns provider capacity until its final frame is yielded or the
    // client drops the body. Ledger completion can happen before that yield.
    let scheduler_lease = reservation.take_scheduler_lease();
    let checkpoint_at = Utc::now();
    reservation
        .checkpoint(cancelled_completion(
            model.clone(),
            provider.clone(),
            attempt.clone(),
            checkpoint_at,
        ))
        .await?;

    let stream = async_stream::stream! {
        let _scheduler_lease = scheduler_lease;
        let mut upstream = response.bytes_stream();
        let mut decoder = EventDecoder::new(limits.event_max_bytes);
        let mut reservation = Some(reservation);
        let mut attempt = attempt;
        let mut first_provider_byte = false;
        let mut first_client_byte = false;
        let mut held = Vec::<Bytes>::new();
        let mut held_bytes = 0usize;
        let mut holding_terminal = false;
        let translated = protocol.encoder.is_some();
        let response_max_bytes = if translated {
            limits.event_max_bytes.saturating_mul(8)
        } else {
            usize::MAX
        };
        let terminal_max_bytes = if translated {
            response_max_bytes
        } else {
            limits.event_max_bytes
        };
        let mut provider_bytes = 0usize;
        let mut client_bytes = 0usize;
        let mut heartbeat = tokio::time::interval(limits.heartbeat_interval);
        heartbeat.tick().await;

        loop {
            let item = tokio::select! {
                item = upstream.next() => item,
                _ = heartbeat.tick() => {
                    mark_client_first_byte(&mut reservation, &mut first_client_byte);
                    yield Ok::<Bytes, io::Error>(Bytes::from_static(b": keepalive\n\n"));
                    continue;
                }
            };
            let Some(item) = item else { break; };
            let bytes = match item {
                Ok(bytes) => bytes,
                Err(error) => {
                    finish_failed(&mut reservation, &mut attempt, &model, &provider, "upstream_body_error").await;
                    yield Err(io::Error::other(error));
                    return;
                }
            };
            provider_bytes = provider_bytes.saturating_add(bytes.len());
            if provider_bytes > response_max_bytes {
                finish_failed(&mut reservation, &mut attempt, &model, &provider, "response_too_large").await;
                yield Err(io::Error::other(ProtocolError::ResponseTooLarge));
                return;
            }
            let mut batch_changed = false;
            if !first_provider_byte {
                first_provider_byte = true;
                attempt.observe_first_byte(Utc::now());
                batch_changed = true;
            }
            let events = match decoder.feed(&bytes) {
                Ok(events) => events,
                Err(error) => {
                    finish_failed(&mut reservation, &mut attempt, &model, &provider, "malformed_sse").await;
                    yield Err(io::Error::other(error));
                    return;
                }
            };
            let mut ready = Vec::new();
            let mut batch_terminal = None;
            for raw in events {
                let observed = match protocol.observer.observe(&raw) {
                    Ok(observed) => observed,
                    Err(error) => {
                        finish_failed(&mut reservation, &mut attempt, &model, &provider, "malformed_sse").await;
                        yield Err(io::Error::other(error));
                        return;
                    }
                };
                let rendered = if let Some(encoder) = protocol.encoder.as_mut() {
                    match encoder.encode(&observed) {
                        Ok(rendered) => rendered,
                        Err(error) => {
                            finish_failed(&mut reservation, &mut attempt, &model, &provider, "malformed_sse").await;
                            yield Err(io::Error::other(error));
                            return;
                        }
                    }
                } else {
                    vec![raw]
                };
                let state = apply_events(&mut attempt, observed, Utc::now());
                batch_changed |= state.changed;
                holding_terminal |= state.hint || state.terminal.is_some();
                for output in rendered {
                    client_bytes = client_bytes.saturating_add(output.len());
                    if client_bytes > response_max_bytes {
                        finish_failed(&mut reservation, &mut attempt, &model, &provider, "response_too_large").await;
                        yield Err(io::Error::other(ProtocolError::ResponseTooLarge));
                        return;
                    }
                    if holding_terminal {
                        held_bytes = held_bytes.saturating_add(output.len());
                        held.push(output);
                        if held_bytes > terminal_max_bytes {
                            finish_failed(&mut reservation, &mut attempt, &model, &provider, "terminal_buffer_overflow").await;
                            yield Err(io::Error::other(ProtocolError::TerminalTooLarge));
                            return;
                        }
                    } else {
                        ready.push(output);
                    }
                }
                if let Some(outcome) = state.terminal {
                    batch_terminal = Some(outcome);
                }
            }
            if let Some(outcome) = batch_terminal {
                if !ready.is_empty() || !held.is_empty() {
                    mark_client_first_byte(&mut reservation, &mut first_client_byte);
                }
                if let Err(error) = finish_terminal(&mut reservation, &mut attempt, &model, &provider, outcome).await {
                    yield Err(io::Error::other(error));
                    return;
                }
                for event in ready {
                    yield Ok::<Bytes, io::Error>(event);
                }
                for terminal in held.drain(..) {
                    yield Ok::<Bytes, io::Error>(terminal);
                }
                return;
            }
            if !ready.is_empty() && !first_client_byte {
                mark_client_first_byte(&mut reservation, &mut first_client_byte);
                batch_changed = true;
            }
            if batch_changed
                && let Err(error) = checkpoint_attempt(
                    &mut reservation,
                    &attempt,
                    &model,
                    &provider,
                ).await
            {
                yield Err(io::Error::other(error));
                return;
            }
            for event in ready {
                yield Ok::<Bytes, io::Error>(event);
            }
        }

        if let Err(error) = decoder.finish() {
            finish_failed(&mut reservation, &mut attempt, &model, &provider, "malformed_sse").await;
            yield Err(io::Error::new(io::ErrorKind::UnexpectedEof, error));
            return;
        }

        let final_events = match protocol.observer.finish() {
            Ok(events) => events,
            Err(error) => {
                finish_failed(&mut reservation, &mut attempt, &model, &provider, "incomplete_stream").await;
                yield Err(io::Error::new(io::ErrorKind::UnexpectedEof, error));
                return;
            }
        };
        let state = apply_events(&mut attempt, final_events, Utc::now());
        let Some(outcome) = state.terminal else {
            finish_failed(&mut reservation, &mut attempt, &model, &provider, "incomplete_stream").await;
            yield Err(io::Error::new(io::ErrorKind::UnexpectedEof, "provider stream ended without a terminal event"));
            return;
        };
        mark_client_first_byte(&mut reservation, &mut first_client_byte);
        if let Err(error) = finish_terminal(&mut reservation, &mut attempt, &model, &provider, outcome).await {
            yield Err(io::Error::other(error));
            return;
        }
        for terminal in held {
            yield Ok::<Bytes, io::Error>(terminal);
        }
    };

    Ok(Body::from_stream(stream))
}

fn mark_client_first_byte(reservation: &mut Option<Reservation>, first_client_byte: &mut bool) {
    if !*first_client_byte {
        if let Some(held) = reservation.as_mut() {
            held.set_client_first_byte_at(Utc::now());
        }
        *first_client_byte = true;
    }
}

#[derive(Clone)]
enum TerminalOutcome {
    Completed,
    Failed(String),
}

#[derive(Default)]
struct AppliedState {
    changed: bool,
    hint: bool,
    terminal: Option<TerminalOutcome>,
}

fn apply_events(
    attempt: &mut ProviderAttempt,
    events: Vec<CanonicalStreamEvent>,
    at: chrono::DateTime<Utc>,
) -> AppliedState {
    let mut state = AppliedState::default();
    for event in events {
        match event {
            CanonicalStreamEvent::ResponseStarted(id) => {
                attempt.observe_response_id(Some(id));
                state.changed = true;
            }
            CanonicalStreamEvent::Usage(usage) => {
                attempt.observe_usage(usage.into(), at);
                state.changed = true;
            }
            CanonicalStreamEvent::CompletionHint => state.hint = true,
            CanonicalStreamEvent::ResponseCompleted => {
                state.terminal = Some(TerminalOutcome::Completed);
            }
            CanonicalStreamEvent::Error(error) => {
                state.terminal = Some(TerminalOutcome::Failed(error.error_type));
            }
            CanonicalStreamEvent::MessageStarted { .. }
            | CanonicalStreamEvent::ContentBlockStarted { .. }
            | CanonicalStreamEvent::TextDelta { .. }
            | CanonicalStreamEvent::ReasoningDelta { .. }
            | CanonicalStreamEvent::ToolCallStarted { .. }
            | CanonicalStreamEvent::ToolArgumentsDelta { .. }
            | CanonicalStreamEvent::ToolCallCompleted { .. }
            | CanonicalStreamEvent::ContentBlockCompleted { .. }
            | CanonicalStreamEvent::MessageCompleted
            | CanonicalStreamEvent::OutputCompleted { .. } => {}
        }
    }
    state
}

async fn checkpoint_attempt(
    reservation: &mut Option<Reservation>,
    attempt: &ProviderAttempt,
    model: &str,
    provider: &str,
) -> Result<(), crate::ledger::LedgerError> {
    let at = Utc::now();
    if let Some(held) = reservation.as_mut() {
        held.checkpoint(cancelled_completion(
            model.to_owned(),
            provider.to_owned(),
            attempt.clone(),
            at,
        ))
        .await?;
    }
    Ok(())
}

async fn finish_terminal(
    reservation: &mut Option<Reservation>,
    attempt: &mut ProviderAttempt,
    model: &str,
    provider: &str,
    outcome: TerminalOutcome,
) -> Result<(), crate::ledger::LedgerError> {
    let at = Utc::now();
    let (request_status, attempt_status, error_type) = match outcome {
        TerminalOutcome::Completed => ("completed", "completed", None),
        TerminalOutcome::Failed(kind) => ("failed", "failed", Some(kind)),
    };
    attempt.conclude(attempt_status, error_type, at);
    if let Some(held) = reservation.take() {
        held.finish(RequestCompletion {
            status: request_status.to_owned(),
            selected_model: Some(model.to_owned()),
            selected_provider: Some(provider.to_owned()),
            attempts: vec![attempt.clone()],
            completed_at: at,
        })
        .await?;
    }
    Ok(())
}

async fn finish_failed(
    reservation: &mut Option<Reservation>,
    attempt: &mut ProviderAttempt,
    model: &str,
    provider: &str,
    error_type: &'static str,
) {
    if let Err(error) = finish_terminal(
        reservation,
        attempt,
        model,
        provider,
        TerminalOutcome::Failed(error_type.to_owned()),
    )
    .await
    {
        tracing::error!(
            error.type = "ledger_failure_not_finalized",
            exception.message = %error,
            "the latest failed stream state could not replace its durable cancellation checkpoint"
        );
    }
}

fn cancelled_completion(
    model: String,
    provider: String,
    attempt: ProviderAttempt,
    completed_at: chrono::DateTime<Utc>,
) -> RequestCompletion {
    RequestCompletion {
        status: "cancelled".to_owned(),
        selected_model: Some(model),
        selected_provider: Some(provider),
        attempts: vec![attempt],
        completed_at,
    }
}

fn event_end(bytes: &[u8]) -> Option<usize> {
    let mut line_start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                if index == line_start {
                    return Some(index + 1);
                }
                index += 1;
                line_start = index;
            }
            b'\r' => {
                let end = if bytes.get(index + 1) == Some(&b'\n') {
                    index + 2
                } else {
                    index + 1
                };
                if index == line_start {
                    return Some(end);
                }
                index = end;
                line_start = end;
            }
            _ => index += 1,
        }
    }
    None
}

fn logical_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        if matches!(bytes[index], b'\r' | b'\n') {
            lines.push(&bytes[start..index]);
            if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                index += 1;
            }
            index += 1;
            start = index;
        } else {
            index += 1;
        }
    }
    if start < bytes.len() {
        lines.push(&bytes[start..]);
    }
    lines
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{EventDecoder, ProtocolError, TerminalOutcome, apply_events, event_data};
    use crate::{
        canonical::{CanonicalContent, CanonicalError, CanonicalStreamEvent, ProviderOptions},
        ledger::{ProviderAttempt, TokenUsage},
    };

    #[test]
    fn frames_split_crlf_and_cr_only_without_rewriting_bytes() {
        let raw = b": ping\r\n\r\ndata: one\r\rdata: two\n\n";
        let mut decoder = EventDecoder::default();
        let mut events = decoder.feed(&raw[..8]).unwrap();
        events.extend(decoder.feed(&raw[8..]).unwrap());
        assert_eq!(events.concat(), raw);
        assert_eq!(event_data(&events[1]).bytes, b"one");
        assert_eq!(event_data(&events[2]).bytes, b"two");
    }

    #[test]
    fn event_data_joins_multiline_payloads() {
        let data = event_data(b"event: value\ndata: {\ndata: \"ok\":true}\n\n");
        assert_eq!(data.bytes, b"{\n\"ok\":true}");
        assert!(data.has_data);
        assert!(data.has_non_comment_field);
    }

    #[test]
    fn eof_does_not_promote_an_unclosed_fragment_to_an_event() {
        let mut decoder = EventDecoder::default();
        assert!(
            decoder
                .feed(b"data: {\"type\":\"response.completed\"}")
                .unwrap()
                .is_empty()
        );
        assert!(decoder.finish().is_err());
    }

    #[test]
    fn event_limit_is_configurable() {
        let mut decoder = EventDecoder::new(8);
        assert!(matches!(
            decoder.feed(b"data: 123\n\n"),
            Err(ProtocolError::EventTooLarge)
        ));
    }

    #[test]
    fn rich_stream_lifecycle_has_one_terminal_state_machine() {
        let mut attempt = ProviderAttempt::begin(
            "provider".to_owned(),
            "model".to_owned(),
            None,
            Utc::now(),
            TokenUsage::default(),
        );
        let state = apply_events(
            &mut attempt,
            vec![
                CanonicalStreamEvent::ContentBlockStarted {
                    index: 0,
                    content: CanonicalContent::Text {
                        text: "sentinel".to_owned(),
                        provider_options: ProviderOptions::new(),
                    },
                },
                CanonicalStreamEvent::ContentBlockCompleted { index: 0 },
                CanonicalStreamEvent::MessageCompleted,
                CanonicalStreamEvent::OutputCompleted {
                    index: 0,
                    finish_reason: "stop".to_owned(),
                },
                CanonicalStreamEvent::ResponseCompleted,
            ],
            Utc::now(),
        );
        assert!(matches!(state.terminal, Some(TerminalOutcome::Completed)));

        let failed = apply_events(
            &mut attempt,
            vec![CanonicalStreamEvent::Error(CanonicalError::provider(
                "provider_stream_error",
            ))],
            Utc::now(),
        );
        assert!(matches!(
            failed.terminal,
            Some(TerminalOutcome::Failed(ref kind)) if kind == "provider_stream_error"
        ));
    }
}
