use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Seek as _, SeekFrom, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Pool, Postgres, types::Json};
use thiserror::Error;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    identity::PrincipalContext,
    quota::{cost_for_usage, total_tokens},
    routing::RoutingLease,
    scheduler::SchedulerLease,
};

const SCHEMA_VERSION: u8 = 1;
pub const MIN_ENTRY_MAX_BYTES: u64 = 32 * 1024;

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("ledger spool configuration is invalid")]
    InvalidCapacity,
    #[error("ledger spool is full")]
    SpoolFull,
    #[error("ledger record exceeds the reserved entry size")]
    RecordTooLarge,
    #[error("ledger spool is unavailable")]
    Io(#[source] std::io::Error),
    #[error("ledger record is invalid")]
    Json(#[source] serde_json::Error),
    #[error("ledger worker is unavailable")]
    Worker(#[source] tokio::task::JoinError),
    #[error("ledger flush gate is closed")]
    FlushGateClosed,
    #[error("usage database is unavailable")]
    Database(#[source] sqlx::Error),
}

#[derive(Clone)]
pub struct Ledger {
    inner: Arc<LedgerInner>,
}

struct LedgerInner {
    pool: Pool<Postgres>,
    spool: Arc<Spool>,
    database_writes_enabled: AtomicBool,
    flush_pending: AtomicBool,
    flush_worker_scheduled: AtomicBool,
    flush_idle: Notify,
    flush_gate: Arc<Semaphore>,
    finalizations_pending: AtomicUsize,
    finalizations_idle: Notify,
}

#[derive(Clone)]
pub struct RequestStart {
    pub request_id: String,
    pub session_id: Option<String>,
    pub trace_id: Option<String>,
    pub requested_model: Option<String>,
    pub workload_class: Option<String>,
    pub urgency: Option<String>,
    pub quality: Option<String>,
    pub budget_class: Option<String>,
    pub estimated_tokens: Option<i64>,
    pub selection: Option<ModelSelectionSnapshot>,
    pub policy: Option<RequestPolicySnapshot>,
    pub principal: PrincipalContext,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct RequestPolicySnapshot {
    pub deadline_at: Option<DateTime<Utc>>,
    pub max_queue_time_ms: Option<i64>,
    pub decision: Value,
}

impl RequestPolicySnapshot {
    /// A rejection that happened before any policy was evaluated -- the body
    /// did not decode, or the model name is not a name. It says so explicitly,
    /// because an empty decision reads like a policy that allowed the request.
    pub fn unevaluated(reason: &str) -> Self {
        Self {
            deadline_at: None,
            max_queue_time_ms: None,
            decision: serde_json::json!({
                "schema_version": 1,
                "evaluated": false,
                "denied": true,
                "denial_reason": reason,
            }),
        }
    }

    pub fn with_routing(mut self, reason: &str) -> Self {
        if let Value::Object(decision) = &mut self.decision {
            decision.insert(
                "routing".to_owned(),
                serde_json::json!({
                    "attempt_kind": "initial",
                    "reason": bounded_text(reason.to_owned()),
                    "same_request_retry": false,
                }),
            );
        }
        self
    }
}

#[derive(Clone)]
pub struct RequestQuotaSnapshot {
    pub reservation_id: Option<Uuid>,
    pub decision: Value,
    pub requires_cost: bool,
}

#[derive(Clone)]
pub struct ModelSelectionSnapshot {
    pub selector_kind: String,
    pub selector_value: String,
    pub alias_id: Option<Uuid>,
    pub alias_version: Option<i64>,
    pub model_id: Uuid,
    pub model_version: i64,
    pub provider_id: Uuid,
    pub provider_version: i64,
    pub provider_model_id: String,
    pub cost_metadata: Value,
}

#[derive(Clone)]
pub struct RequestCompletion {
    pub status: String,
    pub selected_model: Option<String>,
    pub selected_provider: Option<String>,
    pub attempts: Vec<ProviderAttempt>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct ProviderAttempt {
    id: Uuid,
    provider: String,
    model: String,
    provider_request_id: Option<String>,
    provider_response_id: Option<String>,
    started_at: DateTime<Utc>,
    first_token_at: Option<DateTime<Utc>>,
    completed_at: DateTime<Utc>,
    status: String,
    error_type: Option<String>,
    estimated_usage: TokenUsage,
    actual_usage: TokenUsage,
    selection: Option<ModelSelectionSnapshot>,
}

pub struct CompletedAttempt {
    pub provider: String,
    pub model: String,
    pub provider_request_id: Option<String>,
    pub provider_response_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub status: String,
    pub error_type: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub reasoning_tokens: u64,
}

pub struct Reservation {
    ledger: Ledger,
    id: Option<Uuid>,
    envelope: RequestEnvelope,
    _scheduler_lease: Option<SchedulerLease>,
    routing_lease: Option<RoutingLease>,
    force_token: Option<CancellationToken>,
}

#[derive(Clone, Deserialize, Serialize)]
struct RequestEnvelope {
    schema_version: u8,
    #[serde(default)]
    finalized: Option<bool>,
    request: RequestRecord,
    attempts: Vec<AttemptRecord>,
    usage_records: Vec<UsageRecord>,
}

#[derive(Clone, Deserialize, Serialize)]
struct RequestRecord {
    id: Uuid,
    tenant_id: Uuid,
    request_id: String,
    user_id: Option<Uuid>,
    device_id: Option<Uuid>,
    client_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    credential_id: Option<Uuid>,
    session_id: Option<String>,
    trace_id: Option<String>,
    workload_class: Option<String>,
    urgency: Option<String>,
    quality: Option<String>,
    budget_class: Option<String>,
    requested_model: Option<String>,
    selected_model: Option<String>,
    selected_provider: Option<String>,
    estimated_tokens: Option<i64>,
    #[serde(default = "default_operation")]
    operation: String,
    #[serde(default)]
    client_agent_id: Option<String>,
    #[serde(default)]
    parent_agent_id: Option<String>,
    #[serde(default)]
    request_received_at: Option<DateTime<Utc>>,
    #[serde(default)]
    queue_entered_at: Option<DateTime<Utc>>,
    #[serde(default)]
    queue_left_at: Option<DateTime<Utc>>,
    #[serde(default)]
    client_first_byte_at: Option<DateTime<Utc>>,
    #[serde(default)]
    total_latency_ms: Option<i64>,
    #[serde(default)]
    gateway_overhead_ms: Option<i64>,
    #[serde(default)]
    queue_duration_ms: Option<i64>,
    #[serde(default)]
    selector_kind: Option<String>,
    #[serde(default)]
    selector_value: Option<String>,
    #[serde(default)]
    selected_alias_id: Option<Uuid>,
    #[serde(default)]
    selected_alias_version: Option<i64>,
    #[serde(default)]
    selected_model_id: Option<Uuid>,
    #[serde(default)]
    selected_model_version: Option<i64>,
    #[serde(default)]
    selected_provider_id: Option<Uuid>,
    #[serde(default)]
    selected_provider_version: Option<i64>,
    #[serde(default)]
    selected_provider_model_id: Option<String>,
    #[serde(default)]
    selected_cost_metadata: Option<Value>,
    #[serde(default)]
    deadline_at: Option<DateTime<Utc>>,
    #[serde(default)]
    max_queue_time_ms: Option<i64>,
    #[serde(default)]
    policy_decision: Option<Value>,
    #[serde(default)]
    quota_reservation_id: Option<Uuid>,
    #[serde(default)]
    quota_decision: Option<Value>,
    #[serde(default)]
    quota_requires_cost: bool,
    #[serde(default)]
    quota_actual_tokens: Option<i64>,
    #[serde(default)]
    quota_actual_cost_microusd: Option<i64>,
    status: String,
    created_at: DateTime<Utc>,
    started_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Deserialize, Serialize)]
struct AttemptRecord {
    id: Uuid,
    tenant_id: Uuid,
    gateway_request_id: Uuid,
    provider: String,
    model: String,
    provider_request_id: Option<String>,
    provider_response_id: Option<String>,
    attempt_number: i32,
    started_at: DateTime<Utc>,
    first_token_at: Option<DateTime<Utc>>,
    #[serde(default)]
    provider_request_at: Option<DateTime<Utc>>,
    #[serde(default)]
    provider_first_byte_at: Option<DateTime<Utc>>,
    #[serde(default)]
    provider_ttft_ms: Option<i64>,
    completed_at: DateTime<Utc>,
    status: String,
    error_type: Option<String>,
    estimated_usage: TokenUsage,
    actual_usage: TokenUsage,
    #[serde(default)]
    provider_id: Option<Uuid>,
    #[serde(default)]
    provider_version: Option<i64>,
    #[serde(default)]
    model_id: Option<Uuid>,
    #[serde(default)]
    model_version: Option<i64>,
    #[serde(default)]
    provider_model_id: Option<String>,
    #[serde(default)]
    cost_metadata: Option<Value>,
}

#[derive(Clone, Deserialize, Serialize)]
struct UsageRecord {
    id: Uuid,
    tenant_id: Uuid,
    gateway_request_id: Uuid,
    provider_attempt_id: Uuid,
    source: String,
    input_tokens: i64,
    output_tokens: i64,
    cached_input_tokens: i64,
    cache_write_input_tokens: i64,
    reasoning_tokens: i64,
    cost_microusd: Option<i64>,
    recorded_at: DateTime<Utc>,
}

struct Spool {
    directory: PathBuf,
    entry_max_bytes: u64,
    state: Mutex<SpoolState>,
    checkpointing: Mutex<HashSet<Uuid>>,
    checkpoint_idle: Condvar,
}

struct SpoolState {
    slots_used: usize,
    slots_max: usize,
}

impl Ledger {
    pub fn open(
        pool: Pool<Postgres>,
        directory: PathBuf,
        max_bytes: u64,
        entry_max_bytes: u64,
    ) -> Result<Self, LedgerError> {
        let spool = Arc::new(Spool::open(directory, max_bytes, entry_max_bytes)?);
        Ok(Self {
            inner: Arc::new(LedgerInner {
                pool,
                spool,
                database_writes_enabled: AtomicBool::new(true),
                flush_pending: AtomicBool::new(false),
                flush_worker_scheduled: AtomicBool::new(false),
                flush_idle: Notify::new(),
                flush_gate: Arc::new(Semaphore::new(1)),
                finalizations_pending: AtomicUsize::new(0),
                finalizations_idle: Notify::new(),
            }),
        })
    }

    pub async fn reserve(&self, start: RequestStart) -> Result<Reservation, LedgerError> {
        self.reserve_with_quota(start, None, None).await
    }

    pub async fn reserve_with_quota(
        &self,
        start: RequestStart,
        gateway_request_id: Option<Uuid>,
        quota: Option<RequestQuotaSnapshot>,
    ) -> Result<Reservation, LedgerError> {
        let envelope = RequestEnvelope::started_with(start, gateway_request_id, quota);
        let id = envelope.request.id;
        let spool = Arc::clone(&self.inner.spool);
        let stored = envelope.clone();
        tokio::task::spawn_blocking(move || spool.reserve(id, &stored))
            .await
            .map_err(LedgerError::Worker)??;
        Ok(Reservation {
            ledger: self.clone(),
            id: Some(id),
            envelope,
            _scheduler_lease: None,
            routing_lease: None,
            force_token: None,
        })
    }

    pub async fn reject(
        &self,
        start: RequestStart,
        operation: &str,
        client_agent_id: Option<String>,
        parent_agent_id: Option<String>,
    ) -> Result<(), LedgerError> {
        self.reject_with_quota(start, operation, client_agent_id, parent_agent_id, None)
            .await
    }

    pub async fn reject_with_quota(
        &self,
        start: RequestStart,
        operation: &str,
        client_agent_id: Option<String>,
        parent_agent_id: Option<String>,
        quota: Option<RequestQuotaSnapshot>,
    ) -> Result<(), LedgerError> {
        let mut reservation = self.reserve_with_quota(start, None, quota).await?;
        reservation.annotate_protocol(operation, client_agent_id, parent_agent_id);
        reservation
            .finish(RequestCompletion {
                status: "rejected".to_owned(),
                selected_model: None,
                selected_provider: None,
                attempts: Vec::new(),
                completed_at: Utc::now(),
            })
            .await
    }

    pub async fn flush_startup(&self) -> Result<usize, LedgerError> {
        let _permit = Arc::clone(&self.inner.flush_gate)
            .acquire_owned()
            .await
            .map_err(|_| LedgerError::FlushGateClosed)?;
        self.flush_once().await
    }

    pub(crate) async fn wait_for_idle(&self) {
        loop {
            let notified = self.inner.flush_idle.notified();
            if !self.inner.flush_worker_scheduled.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    pub(crate) async fn wait_for_finalizations(&self) {
        loop {
            let notified = self.inner.finalizations_idle.notified();
            if self.pending_finalizations() == 0 {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn pending_finalizations(&self) -> usize {
        self.inner.finalizations_pending.load(Ordering::Acquire)
    }

    async fn flush_once(&self) -> Result<usize, LedgerError> {
        let spool = Arc::clone(&self.inner.spool);
        let ready = tokio::task::spawn_blocking(move || spool.ready())
            .await
            .map_err(LedgerError::Worker)??;
        if ready.is_empty() {
            return Ok(0);
        }

        let paths = ready
            .iter()
            .map(|entry| entry.0.clone())
            .collect::<Vec<_>>();
        let envelopes = ready.into_iter().map(|entry| entry.1).collect::<Vec<_>>();
        sqlx::query("SELECT ingest_usage_envelopes($1)")
            .bind(Json(envelopes))
            .execute(&self.inner.pool)
            .await
            .map_err(LedgerError::Database)?;

        let flushed = paths.len();
        let spool = Arc::clone(&self.inner.spool);
        tokio::task::spawn_blocking(move || spool.remove(&paths))
            .await
            .map_err(LedgerError::Worker)??;
        Ok(flushed)
    }

    fn schedule_flush(&self, runtime: &tokio::runtime::Handle) {
        if !self.inner.database_writes_enabled.load(Ordering::Acquire) {
            return;
        }
        self.inner.flush_pending.store(true, Ordering::Release);
        if self
            .inner
            .flush_worker_scheduled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let ledger = self.clone();
        let runtime = runtime.clone();
        let task_runtime = runtime.clone();
        runtime.spawn(async move {
            let permit = Arc::clone(&ledger.inner.flush_gate).acquire_owned().await;
            if let Ok(permit) = permit {
                let should_flush = ledger.inner.flush_pending.swap(false, Ordering::AcqRel)
                    && ledger.inner.database_writes_enabled.load(Ordering::Acquire);
                if should_flush && let Err(error) = ledger.flush_once().await {
                    ledger
                        .inner
                        .database_writes_enabled
                        .store(false, Ordering::Release);
                    tracing::error!(
                        error.type = "ledger_flush_failed",
                        exception.message = %error,
                        "usage remains in the durable spool; restart after database recovery"
                    );
                }
                drop(permit);
            } else {
                tracing::error!(
                    error.type = "ledger_flush_gate_closed",
                    "usage remains in the durable spool"
                );
            }

            ledger
                .inner
                .flush_worker_scheduled
                .store(false, Ordering::Release);
            ledger.inner.flush_idle.notify_waiters();
            if ledger.inner.flush_pending.load(Ordering::Acquire)
                && ledger.inner.database_writes_enabled.load(Ordering::Acquire)
            {
                ledger.schedule_flush(&task_runtime);
            }
        });
    }
}

impl Reservation {
    pub fn cancel_on_force(&mut self, token: CancellationToken) {
        self.force_token = Some(token);
    }

    pub async fn checkpoint_provider_attempt(
        &mut self,
        model: String,
        provider: String,
        attempt: ProviderAttempt,
    ) -> Result<(), LedgerError> {
        self.checkpoint(RequestCompletion {
            status: "cancelled".to_owned(),
            selected_model: Some(model),
            selected_provider: Some(provider),
            attempts: vec![attempt],
            completed_at: Utc::now(),
        })
        .await
    }

    pub fn attach_routing_lease(&mut self, lease: RoutingLease) {
        self.routing_lease = Some(lease);
    }

    pub fn attach_scheduler_lease(&mut self, lease: SchedulerLease) {
        self._scheduler_lease = Some(lease);
    }

    pub fn take_scheduler_lease(&mut self) -> Option<SchedulerLease> {
        self._scheduler_lease.take()
    }

    pub fn annotate_selection(&mut self, selection: &ModelSelectionSnapshot) {
        self.envelope.request.selector_kind = Some(selection.selector_kind.clone());
        self.envelope.request.selector_value = Some(selection.selector_value.clone());
        self.envelope.request.selected_alias_id = selection.alias_id;
        self.envelope.request.selected_alias_version = selection.alias_version;
        self.envelope.request.selected_model_id = Some(selection.model_id);
        self.envelope.request.selected_model_version = Some(selection.model_version);
        self.envelope.request.selected_provider_id = Some(selection.provider_id);
        self.envelope.request.selected_provider_version = Some(selection.provider_version);
        self.envelope.request.selected_provider_model_id =
            Some(selection.provider_model_id.clone());
        self.envelope.request.selected_cost_metadata = Some(selection.cost_metadata.clone());
    }

    pub fn annotate_protocol(
        &mut self,
        operation: &str,
        client_agent_id: Option<String>,
        parent_agent_id: Option<String>,
    ) {
        self.envelope.request.operation = bounded_text(operation.to_owned());
        self.envelope.request.client_agent_id = bounded(client_agent_id);
        self.envelope.request.parent_agent_id = bounded(parent_agent_id);
    }

    pub fn annotate_policy(&mut self, policy: RequestPolicySnapshot) {
        self.envelope.request.deadline_at = policy.deadline_at;
        self.envelope.request.max_queue_time_ms = policy.max_queue_time_ms;
        self.envelope.request.policy_decision = Some(policy.decision);
    }

    pub fn set_client_first_byte_at(&mut self, at: DateTime<Utc>) {
        self.envelope.request.client_first_byte_at.get_or_insert(at);
    }

    pub fn set_deadline_at(&mut self, at: DateTime<Utc>) {
        self.envelope.request.deadline_at = Some(at);
    }

    pub fn annotate_quota_scheduler_outcome(&mut self, outcome: &str) {
        if let Some(Value::Object(decision)) = self.envelope.request.quota_decision.as_mut() {
            decision.insert(
                "scheduler_outcome".to_owned(),
                Value::String(outcome.to_owned()),
            );
        }
    }

    pub async fn checkpoint_queue_entered(&mut self, at: DateTime<Utc>) -> Result<(), LedgerError> {
        if self.envelope.request.queue_entered_at.is_some()
            || self.envelope.request.queue_left_at.is_some()
        {
            return Err(LedgerError::InvalidCapacity);
        }
        self.envelope.request.queue_entered_at = Some(at);
        self.checkpoint_current().await
    }

    pub async fn checkpoint_queue_left(&mut self, at: DateTime<Utc>) -> Result<(), LedgerError> {
        let entered = self
            .envelope
            .request
            .queue_entered_at
            .ok_or(LedgerError::InvalidCapacity)?;
        if self.envelope.request.queue_left_at.is_some() || at < entered {
            return Err(LedgerError::InvalidCapacity);
        }
        self.envelope.request.queue_left_at = Some(at);
        self.envelope.request.queue_duration_ms = Some((at - entered).num_milliseconds());
        self.checkpoint_current().await
    }

    async fn checkpoint_current(&self) -> Result<(), LedgerError> {
        let id = self.id.ok_or(LedgerError::InvalidCapacity)?;
        let spool = Arc::clone(&self.ledger.inner.spool);
        spool.begin_checkpoint(id)?;
        let envelope = self.envelope.clone();
        tokio::task::spawn_blocking(move || {
            let _checkpoint = CheckpointGuard {
                spool: Arc::clone(&spool),
                id,
            };
            spool.checkpoint(id, &envelope)
        })
        .await
        .map_err(LedgerError::Worker)??;
        Ok(())
    }

    /// Durably replaces the interruption fallback without publishing it for
    /// database ingestion. If the response body is dropped, `Drop` publishes
    /// this latest complete checkpoint instead of the initial empty record.
    pub async fn checkpoint(&mut self, completion: RequestCompletion) -> Result<(), LedgerError> {
        self.envelope.complete(completion);
        self.checkpoint_current().await
    }

    pub async fn finish(mut self, completion: RequestCompletion) -> Result<(), LedgerError> {
        if let Some(lease) = self.routing_lease.as_mut() {
            let error_type = completion
                .attempts
                .last()
                .and_then(|attempt| attempt.error_type.as_deref());
            lease.conclude(
                completion.completed_at,
                completion.status == "completed",
                error_type,
            );
        }
        // Keep ownership while the final envelope becomes the durable
        // checkpoint. Cancellation during that write still publishes a
        // cancellation record; after it returns, recovery can always promote
        // the final checkpoint even if publishing `.ready` fails.
        self.envelope.finalized = Some(true);
        self.checkpoint(completion).await?;
        let id = self.id.take().ok_or(LedgerError::InvalidCapacity)?;
        let spool = Arc::clone(&self.ledger.inner.spool);
        let ledger = self.ledger.clone();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let result = spool.publish(id);
            if result.is_ok() {
                ledger.schedule_flush(&runtime);
            }
            result
        })
        .await
        .map_err(LedgerError::Worker)??;
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        let forced = self
            .force_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled);
        self.envelope
            .cancel(Utc::now(), forced.then_some("shutdown_timeout"));
        let envelope = self.envelope.clone();
        let ledger = self.ledger.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let flush_handle = handle.clone();
                ledger
                    .inner
                    .finalizations_pending
                    .fetch_add(1, Ordering::AcqRel);
                handle.spawn_blocking(move || {
                    match ledger.inner.spool.finish(id, &envelope) {
                        Ok(()) => {
                            ledger.schedule_flush(&flush_handle);
                        }
                        Err(error) => tracing::error!(
                            error.type = "ledger_interrupted_request_not_finalized",
                            exception.message = %error,
                            "an interrupted request remains reserved in the spool"
                        ),
                    }
                    if ledger
                        .inner
                        .finalizations_pending
                        .fetch_sub(1, Ordering::AcqRel)
                        == 1
                    {
                        ledger.inner.finalizations_idle.notify_waiters();
                    }
                });
            }
            Err(error) => tracing::error!(
                error.type = "ledger_runtime_unavailable",
                exception.message = %error,
                "an interrupted request remains reserved until restart"
            ),
        }
    }
}

impl ProviderAttempt {
    pub fn begin(
        provider: String,
        model: String,
        provider_request_id: Option<String>,
        started_at: DateTime<Utc>,
        estimated_usage: TokenUsage,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            provider: bounded_text(provider),
            model: bounded_text(model),
            provider_request_id: bounded(provider_request_id),
            provider_response_id: None,
            started_at,
            first_token_at: None,
            completed_at: started_at,
            status: "cancelled".to_owned(),
            error_type: Some("cancelled".to_owned()),
            estimated_usage,
            actual_usage: TokenUsage::default(),
            selection: None,
        }
    }

    pub fn observe_request_id(&mut self, provider_request_id: Option<String>) {
        self.provider_request_id = bounded(provider_request_id);
    }

    pub fn with_selection(mut self, selection: Option<ModelSelectionSnapshot>) -> Self {
        self.selection = selection;
        self
    }

    pub fn observe_first_byte(&mut self, at: DateTime<Utc>) {
        self.first_token_at.get_or_insert(at);
        self.completed_at = at;
    }

    pub fn observe_response_id(&mut self, id: Option<String>) {
        if id.is_some() {
            self.provider_response_id = bounded(id);
        }
    }

    pub fn observe_usage(&mut self, usage: TokenUsage, at: DateTime<Utc>) {
        self.actual_usage = usage;
        self.completed_at = at;
    }

    pub fn conclude(&mut self, status: &str, error_type: Option<String>, at: DateTime<Utc>) {
        self.status = bounded_text(status.to_owned());
        self.error_type = bounded(error_type);
        self.completed_at = at;
    }

    pub fn completed(attempt: CompletedAttempt, response_body: &[u8]) -> Self {
        Self {
            id: Uuid::new_v4(),
            provider: bounded_text(attempt.provider),
            model: bounded_text(attempt.model),
            provider_request_id: bounded(attempt.provider_request_id),
            provider_response_id: bounded(attempt.provider_response_id),
            started_at: attempt.started_at,
            first_token_at: Some(attempt.completed_at),
            completed_at: attempt.completed_at,
            status: bounded_text(attempt.status),
            error_type: bounded(attempt.error_type),
            estimated_usage: TokenUsage::default(),
            actual_usage: TokenUsage::from_response(response_body),
            selection: None,
        }
    }

    pub fn failed(
        provider: String,
        model: String,
        started_at: DateTime<Utc>,
        completed_at: DateTime<Utc>,
        error_type: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            provider: bounded_text(provider),
            model: bounded_text(model),
            provider_request_id: None,
            provider_response_id: None,
            started_at,
            first_token_at: None,
            completed_at,
            status: "failed".to_owned(),
            error_type: Some(bounded_text(error_type)),
            estimated_usage: TokenUsage::default(),
            actual_usage: TokenUsage::default(),
            selection: None,
        }
    }
}

impl TokenUsage {
    pub fn from_response(body: &[u8]) -> Self {
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return Self::default();
        };
        let usage = &value["usage"];
        Self {
            input_tokens: first_u64(usage, &["input_tokens", "prompt_tokens"]),
            output_tokens: first_u64(usage, &["output_tokens", "completion_tokens"]),
            cached_input_tokens: first_u64(
                usage,
                &["cache_read_input_tokens", "cached_input_tokens"],
            )
            .max(value_at(usage, &["prompt_tokens_details", "cached_tokens"]))
            .max(value_at(usage, &["input_tokens_details", "cached_tokens"])),
            cache_write_input_tokens: first_u64(
                usage,
                &["cache_creation_input_tokens", "cache_write_input_tokens"],
            )
            .max(value_at(
                usage,
                &["input_tokens_details", "cache_write_tokens"],
            ))
            .max(value_at(
                usage,
                &["prompt_tokens_details", "cache_write_tokens"],
            )),
            reasoning_tokens: first_u64(usage, &["reasoning_tokens"]).max(value_at(
                usage,
                &["output_tokens_details", "reasoning_tokens"],
            )),
        }
    }

    pub fn from_value(usage: &Value) -> Self {
        Self {
            input_tokens: first_u64(usage, &["input_tokens", "prompt_tokens"]),
            output_tokens: first_u64(usage, &["output_tokens", "completion_tokens"]),
            cached_input_tokens: first_u64(
                usage,
                &["cache_read_input_tokens", "cached_input_tokens"],
            )
            .max(value_at(usage, &["prompt_tokens_details", "cached_tokens"]))
            .max(value_at(usage, &["input_tokens_details", "cached_tokens"])),
            cache_write_input_tokens: first_u64(
                usage,
                &["cache_creation_input_tokens", "cache_write_input_tokens"],
            )
            .max(value_at(
                usage,
                &["input_tokens_details", "cache_write_tokens"],
            ))
            .max(value_at(
                usage,
                &["prompt_tokens_details", "cache_write_tokens"],
            )),
            reasoning_tokens: first_u64(usage, &["reasoning_tokens"]).max(value_at(
                usage,
                &["output_tokens_details", "reasoning_tokens"],
            )),
        }
    }

    fn is_empty(&self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.cached_input_tokens == 0
            && self.cache_write_input_tokens == 0
            && self.reasoning_tokens == 0
    }
}

impl RequestEnvelope {
    #[cfg(test)]
    fn started(start: RequestStart) -> Self {
        Self::started_with(start, None, None)
    }

    fn started_with(
        start: RequestStart,
        gateway_request_id: Option<Uuid>,
        quota: Option<RequestQuotaSnapshot>,
    ) -> Self {
        let request_id = gateway_request_id.unwrap_or_else(Uuid::new_v4);
        let credential_id =
            (start.principal.credential_id != Uuid::nil()).then_some(start.principal.credential_id);
        let selection = start.selection;
        let policy = start.policy;
        Self {
            schema_version: SCHEMA_VERSION,
            finalized: Some(false),
            request: RequestRecord {
                id: request_id,
                tenant_id: start.principal.tenant_id,
                request_id: nonempty_bounded(start.request_id),
                user_id: start.principal.user_id,
                device_id: start.principal.device_id,
                client_id: start.principal.client_id,
                agent_id: start.principal.agent_id,
                credential_id,
                session_id: bounded(start.session_id),
                trace_id: bounded(start.trace_id),
                workload_class: bounded(start.workload_class),
                urgency: bounded(start.urgency),
                quality: bounded(start.quality),
                budget_class: bounded(start.budget_class),
                requested_model: bounded(start.requested_model),
                selected_model: None,
                selected_provider: None,
                estimated_tokens: start.estimated_tokens,
                operation: default_operation(),
                client_agent_id: None,
                parent_agent_id: None,
                request_received_at: Some(start.created_at),
                queue_entered_at: None,
                queue_left_at: None,
                client_first_byte_at: None,
                total_latency_ms: None,
                gateway_overhead_ms: None,
                queue_duration_ms: None,
                selector_kind: selection.as_ref().map(|value| value.selector_kind.clone()),
                selector_value: selection.as_ref().map(|value| value.selector_value.clone()),
                selected_alias_id: selection.as_ref().and_then(|value| value.alias_id),
                selected_alias_version: selection.as_ref().and_then(|value| value.alias_version),
                selected_model_id: selection.as_ref().map(|value| value.model_id),
                selected_model_version: selection.as_ref().map(|value| value.model_version),
                selected_provider_id: selection.as_ref().map(|value| value.provider_id),
                selected_provider_version: selection.as_ref().map(|value| value.provider_version),
                selected_provider_model_id: selection
                    .as_ref()
                    .map(|value| value.provider_model_id.clone()),
                selected_cost_metadata: selection.map(|value| value.cost_metadata),
                deadline_at: policy.as_ref().and_then(|value| value.deadline_at),
                max_queue_time_ms: policy.as_ref().and_then(|value| value.max_queue_time_ms),
                policy_decision: policy.map(|value| value.decision),
                quota_reservation_id: quota.as_ref().and_then(|value| value.reservation_id),
                quota_decision: quota.as_ref().map(|value| value.decision.clone()),
                quota_requires_cost: quota.as_ref().is_some_and(|value| value.requires_cost),
                quota_actual_tokens: None,
                quota_actual_cost_microusd: None,
                status: "interrupted".to_owned(),
                created_at: start.created_at,
                started_at: start.created_at,
                completed_at: None,
            },
            attempts: Vec::new(),
            usage_records: Vec::new(),
        }
    }

    fn complete(&mut self, completion: RequestCompletion) {
        self.request.status = bounded_text(completion.status);
        self.request.selected_model = bounded(completion.selected_model);
        self.request.selected_provider = bounded(completion.selected_provider);
        self.request.completed_at = Some(completion.completed_at);
        self.request.total_latency_ms = self
            .request
            .request_received_at
            .map(|received| (completion.completed_at - received).num_milliseconds());
        let actual_attempts = completion
            .attempts
            .iter()
            .filter(|attempt| !attempt.actual_usage.is_empty())
            .collect::<Vec<_>>();
        if completion.attempts.is_empty() && self.request.quota_reservation_id.is_some() {
            self.request.quota_actual_tokens = Some(0);
            self.request.quota_actual_cost_microusd = Some(0);
        }
        if self.request.operation == "count_tokens" && self.request.quota_reservation_id.is_some() {
            self.request.quota_actual_tokens = Some(0);
            self.request.quota_actual_cost_microusd = Some(0);
        }
        if !actual_attempts.is_empty() {
            self.request.quota_actual_tokens = actual_attempts
                .iter()
                .try_fold(0_u64, |total, attempt| {
                    total.checked_add(total_tokens(&attempt.actual_usage).ok()?)
                })
                .and_then(|value| i64::try_from(value).ok());
            self.request.quota_actual_cost_microusd = if self.request.quota_requires_cost {
                actual_attempts
                    .iter()
                    .try_fold(0_u64, |total, attempt| {
                        let metadata = &attempt.selection.as_ref()?.cost_metadata;
                        total.checked_add(cost_for_usage(metadata, &attempt.actual_usage).ok()?)
                    })
                    .and_then(|value| i64::try_from(value).ok())
            } else {
                Some(0)
            };
        }
        self.attempts = completion
            .attempts
            .into_iter()
            .enumerate()
            .map(|(index, attempt)| AttemptRecord {
                id: attempt.id,
                tenant_id: self.request.tenant_id,
                gateway_request_id: self.request.id,
                provider: attempt.provider,
                model: attempt.model,
                provider_request_id: attempt.provider_request_id,
                provider_response_id: attempt.provider_response_id,
                attempt_number: i32::try_from(index + 1).unwrap_or(i32::MAX),
                started_at: attempt.started_at,
                first_token_at: attempt.first_token_at,
                provider_request_at: Some(attempt.started_at),
                provider_first_byte_at: attempt.first_token_at,
                provider_ttft_ms: attempt
                    .first_token_at
                    .map(|first| (first - attempt.started_at).num_milliseconds()),
                completed_at: attempt.completed_at,
                status: attempt.status,
                error_type: attempt.error_type,
                estimated_usage: attempt.estimated_usage,
                actual_usage: attempt.actual_usage,
                provider_id: attempt.selection.as_ref().map(|value| value.provider_id),
                provider_version: attempt
                    .selection
                    .as_ref()
                    .map(|value| value.provider_version),
                model_id: attempt.selection.as_ref().map(|value| value.model_id),
                model_version: attempt.selection.as_ref().map(|value| value.model_version),
                provider_model_id: attempt
                    .selection
                    .as_ref()
                    .map(|value| value.provider_model_id.clone()),
                cost_metadata: attempt
                    .selection
                    .as_ref()
                    .map(|value| value.cost_metadata.clone()),
            })
            .collect();

        self.request.gateway_overhead_ms = self
            .request
            .client_first_byte_at
            .zip(
                self.attempts
                    .first()
                    .and_then(|attempt| attempt.provider_first_byte_at),
            )
            .map(|(client, provider)| (client - provider).num_milliseconds());

        self.usage_records.clear();
        for attempt in &self.attempts {
            if !attempt.estimated_usage.is_empty() {
                self.usage_records.push(UsageRecord::from_usage(
                    &self.request,
                    attempt,
                    "estimated",
                    &attempt.estimated_usage,
                ));
            }
            if !attempt.actual_usage.is_empty() {
                self.usage_records.push(UsageRecord::from_usage(
                    &self.request,
                    attempt,
                    "provider_reported",
                    &attempt.actual_usage,
                ));
            }
        }
    }

    fn cancel(&mut self, cancelled_at: DateTime<Utc>, error_type: Option<&str>) {
        self.finalized = Some(true);
        self.request.status = "cancelled".to_owned();
        self.request.completed_at = Some(cancelled_at);
        self.request.total_latency_ms = self
            .request
            .request_received_at
            .map(|received| (cancelled_at - received).num_milliseconds());
        if self.attempts.is_empty() && self.request.quota_reservation_id.is_some() {
            self.request.quota_actual_tokens = Some(0);
            self.request.quota_actual_cost_microusd = Some(0);
        } else if !self.attempts.is_empty() {
            // A cancelled provider attempt may contain only an early usage
            // fragment. Keep that fragment as evidence, but do not reconcile
            // the conservative quota reservation downward as if it were final.
            self.request.quota_actual_tokens = None;
            self.request.quota_actual_cost_microusd = None;
        }
        for attempt in &mut self.attempts {
            if attempt.status == "cancelled" {
                attempt.completed_at = cancelled_at;
                if let Some(error_type) = error_type {
                    attempt.error_type = Some(error_type.to_owned());
                }
            }
        }
    }
}

impl UsageRecord {
    fn from_usage(
        request: &RequestRecord,
        attempt: &AttemptRecord,
        source: &str,
        usage: &TokenUsage,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            tenant_id: request.tenant_id,
            gateway_request_id: request.id,
            provider_attempt_id: attempt.id,
            source: source.to_owned(),
            input_tokens: to_i64(usage.input_tokens),
            output_tokens: to_i64(usage.output_tokens),
            cached_input_tokens: to_i64(usage.cached_input_tokens),
            cache_write_input_tokens: to_i64(usage.cache_write_input_tokens),
            reasoning_tokens: to_i64(usage.reasoning_tokens),
            cost_microusd: attempt
                .cost_metadata
                .as_ref()
                .and_then(|metadata| cost_for_usage(metadata, usage).ok())
                .and_then(|value| i64::try_from(value).ok()),
            recorded_at: attempt.completed_at,
        }
    }
}

impl Spool {
    fn open(directory: PathBuf, max_bytes: u64, entry_max_bytes: u64) -> Result<Self, LedgerError> {
        let slot_bytes = entry_max_bytes
            .checked_mul(3)
            .ok_or(LedgerError::InvalidCapacity)?;
        if entry_max_bytes < MIN_ENTRY_MAX_BYTES || max_bytes < slot_bytes {
            return Err(LedgerError::InvalidCapacity);
        }
        fs::create_dir_all(&directory).map_err(LedgerError::Io)?;
        let mut states: HashMap<String, HashSet<String>> = HashMap::new();
        for entry in fs::read_dir(&directory).map_err(LedgerError::Io)? {
            let path = entry.map_err(LedgerError::Io)?.path();
            let extension = path.extension().and_then(|value| value.to_str());
            match extension {
                Some("tmp") => fs::remove_file(path).map_err(LedgerError::Io)?,
                Some(extension @ ("reserved" | "working" | "next" | "ready")) => {
                    let stem = path
                        .file_stem()
                        .and_then(|value| value.to_str())
                        .ok_or(LedgerError::InvalidCapacity)?;
                    states
                        .entry(stem.to_owned())
                        .or_default()
                        .insert(extension.to_owned());
                }
                _ => {}
            }
        }

        let mut ids = HashSet::new();
        for (stem, extensions) in states {
            let ready = directory.join(format!("{stem}.ready"));
            let reserved = directory.join(format!("{stem}.reserved"));
            let working = directory.join(format!("{stem}.working"));
            let next = directory.join(format!("{stem}.next"));
            if extensions.contains("ready") {
                finalize_recovered_envelope(&ready, entry_max_bytes)?;
                remove_if_present(&reserved)?;
                remove_if_present(&working)?;
                remove_if_present(&next)?;
                ids.insert(stem);
            } else if extensions.contains("reserved") {
                if extensions.contains("working") && self_contained_envelope(&working)? {
                    fs::rename(&working, &ready).map_err(LedgerError::Io)?;
                    remove_if_present(&reserved)?;
                } else {
                    remove_if_present(&working)?;
                    fs::rename(&reserved, &ready).map_err(LedgerError::Io)?;
                }
                finalize_recovered_envelope(&ready, entry_max_bytes)?;
                remove_if_present(&next)?;
                ids.insert(stem);
            } else {
                remove_if_present(&working)?;
                remove_if_present(&next)?;
            }
        }
        File::open(&directory)
            .and_then(|value| value.sync_all())
            .map_err(LedgerError::Io)?;

        // The spool is the durability guarantee for every accepted request, so
        // a directory this process cannot write is a broken deployment. Prove
        // it at startup: a container that mounts the volume as another user
        // stays healthy on every later check and only loses the ledger.
        let probe = directory.join("startup.tmp");
        fs::write(&probe, b"").map_err(LedgerError::Io)?;
        remove_if_present(&probe)?;

        // One accepted request reserves its interrupted record and two
        // checkpoint workspaces. One remains the last good checkpoint while
        // the other is overwritten, so a torn write cannot destroy both.
        // Completed entries use less disk,
        // but retaining the conservative slot count guarantees the next
        // request can always finish without allocating after provider cost.
        let slots_max =
            usize::try_from(max_bytes / slot_bytes).map_err(|_| LedgerError::InvalidCapacity)?;
        if ids.len() > slots_max {
            return Err(LedgerError::SpoolFull);
        }
        Ok(Self {
            directory,
            entry_max_bytes,
            state: Mutex::new(SpoolState {
                slots_used: ids.len(),
                slots_max,
            }),
            checkpointing: Mutex::new(HashSet::new()),
            checkpoint_idle: Condvar::new(),
        })
    }

    fn begin_checkpoint(&self, id: Uuid) -> Result<(), LedgerError> {
        self.checkpointing
            .lock()
            .map_err(|_| LedgerError::InvalidCapacity)?
            .insert(id);
        Ok(())
    }

    fn end_checkpoint(&self, id: Uuid) {
        if let Ok(mut active) = self.checkpointing.lock() {
            active.remove(&id);
            self.checkpoint_idle.notify_all();
        }
    }

    fn wait_for_checkpoint(&self, id: Uuid) -> Result<(), LedgerError> {
        let mut active = self
            .checkpointing
            .lock()
            .map_err(|_| LedgerError::InvalidCapacity)?;
        while active.contains(&id) {
            active = self
                .checkpoint_idle
                .wait(active)
                .map_err(|_| LedgerError::InvalidCapacity)?;
        }
        Ok(())
    }

    fn reserve(&self, id: Uuid, envelope: &RequestEnvelope) -> Result<(), LedgerError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| LedgerError::InvalidCapacity)?;
        if state.slots_used >= state.slots_max {
            return Err(LedgerError::SpoolFull);
        }
        let mut bytes = self.encode(envelope)?;
        bytes.resize(
            usize::try_from(self.entry_max_bytes).map_err(|_| LedgerError::InvalidCapacity)?,
            0,
        );
        let target = self.path(id, "reserved");
        self.atomic_replace(&target, &bytes)?;
        let working = self.path(id, "working");
        if let Err(error) = self.create_preallocated(&working) {
            remove_if_present(&target)?;
            remove_if_present(&working)?;
            self.sync_directory()?;
            return Err(error);
        }
        let next = self.path(id, "next");
        if let Err(error) = self.create_preallocated(&next) {
            remove_if_present(&target)?;
            remove_if_present(&working)?;
            remove_if_present(&next)?;
            self.sync_directory()?;
            return Err(error);
        }
        self.sync_directory()?;
        state.slots_used += 1;
        Ok(())
    }

    fn finish(&self, id: Uuid, envelope: &RequestEnvelope) -> Result<(), LedgerError> {
        self.wait_for_checkpoint(id)?;
        self.checkpoint(id, envelope)?;
        self.publish(id)
    }

    fn publish(&self, id: Uuid) -> Result<(), LedgerError> {
        let reserved = self.path(id, "reserved");
        let working = self.path(id, "working");
        fs::rename(&working, self.path(id, "ready")).map_err(LedgerError::Io)?;
        remove_if_present(&reserved)?;
        remove_if_present(&self.path(id, "next"))?;
        self.sync_directory()
    }

    fn checkpoint(&self, id: Uuid, envelope: &RequestEnvelope) -> Result<(), LedgerError> {
        let mut bytes = self.encode(envelope)?;
        bytes.resize(
            usize::try_from(self.entry_max_bytes).map_err(|_| LedgerError::InvalidCapacity)?,
            0,
        );
        let working = self.path(id, "working");
        let next = self.path(id, "next");
        let mut file = OpenOptions::new()
            .write(true)
            .open(&next)
            .map_err(LedgerError::Io)?;
        file.seek(SeekFrom::Start(0)).map_err(LedgerError::Io)?;
        file.write_all(&bytes).map_err(LedgerError::Io)?;
        file.sync_all().map_err(LedgerError::Io)?;
        drop(file);
        fs::rename(&next, &working).map_err(LedgerError::Io)?;
        self.sync_directory()?;
        self.create_preallocated(&next)?;
        self.sync_directory()
    }

    fn ready(&self) -> Result<Vec<(PathBuf, RequestEnvelope)>, LedgerError> {
        let mut paths = Vec::new();
        for entry in fs::read_dir(&self.directory).map_err(LedgerError::Io)? {
            let path = entry.map_err(LedgerError::Io)?.path();
            if path.extension().and_then(|value| value.to_str()) == Some("ready") {
                paths.push(path);
            }
        }
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let bytes = fs::read(&path).map_err(LedgerError::Io)?;
                let end = bytes
                    .iter()
                    .rposition(|byte| *byte != 0)
                    .map_or(0, |index| index + 1);
                let envelope: RequestEnvelope =
                    serde_json::from_slice(&bytes[..end]).map_err(LedgerError::Json)?;
                if envelope.schema_version != SCHEMA_VERSION {
                    return Err(LedgerError::InvalidCapacity);
                }
                Ok((path, envelope))
            })
            .collect()
    }

    fn remove(&self, paths: &[PathBuf]) -> Result<(), LedgerError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| LedgerError::InvalidCapacity)?;
        for path in paths {
            fs::remove_file(path).map_err(LedgerError::Io)?;
            state.slots_used = state.slots_used.saturating_sub(1);
        }
        self.sync_directory()
    }

    fn encode(&self, envelope: &RequestEnvelope) -> Result<Vec<u8>, LedgerError> {
        let bytes = serde_json::to_vec(envelope).map_err(LedgerError::Json)?;
        if u64::try_from(bytes.len()).map_or(true, |length| length > self.entry_max_bytes) {
            return Err(LedgerError::RecordTooLarge);
        }
        Ok(bytes)
    }

    fn atomic_replace(&self, target: &Path, bytes: &[u8]) -> Result<(), LedgerError> {
        let temporary = self.path(Uuid::new_v4(), "tmp");
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(LedgerError::Io)?;
            file.write_all(bytes).map_err(LedgerError::Io)?;
            file.sync_all().map_err(LedgerError::Io)?;
            drop(file);
            fs::rename(&temporary, target).map_err(LedgerError::Io)?;
            self.sync_directory()
        })();
        if result.is_err() {
            let _ = remove_if_present(&temporary);
        }
        result
    }

    fn create_preallocated(&self, target: &Path) -> Result<(), LedgerError> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(target)
            .map_err(LedgerError::Io)?;
        let bytes = vec![
            0;
            usize::try_from(self.entry_max_bytes)
                .map_err(|_| LedgerError::InvalidCapacity)?
        ];
        file.write_all(&bytes).map_err(LedgerError::Io)?;
        file.sync_all().map_err(LedgerError::Io)
    }

    fn sync_directory(&self) -> Result<(), LedgerError> {
        File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(LedgerError::Io)
    }

    fn path(&self, id: Uuid, extension: &str) -> PathBuf {
        self.directory
            .join(format!("{}.{}", id.simple(), extension))
    }
}

struct CheckpointGuard {
    spool: Arc<Spool>,
    id: Uuid,
}

impl Drop for CheckpointGuard {
    fn drop(&mut self) {
        self.spool.end_checkpoint(self.id);
    }
}

fn self_contained_envelope(path: &Path) -> Result<bool, LedgerError> {
    let bytes = fs::read(path).map_err(LedgerError::Io)?;
    let end = bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |index| index + 1);
    if end == 0 {
        return Ok(false);
    }
    Ok(serde_json::from_slice::<RequestEnvelope>(&bytes[..end])
        .is_ok_and(|envelope| envelope.schema_version == SCHEMA_VERSION))
}

fn finalize_recovered_envelope(path: &Path, entry_max_bytes: u64) -> Result<(), LedgerError> {
    let bytes = fs::read(path).map_err(LedgerError::Io)?;
    let end = bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |index| index + 1);
    let mut envelope: RequestEnvelope =
        serde_json::from_slice(&bytes[..end]).map_err(LedgerError::Json)?;
    if envelope.schema_version != SCHEMA_VERSION {
        return Err(LedgerError::InvalidCapacity);
    }
    let needs_finalization = match envelope.finalized {
        Some(finalized) => !finalized,
        None => {
            envelope.request.completed_at.is_none()
                || (envelope.request.status == "cancelled"
                    && envelope
                        .attempts
                        .iter()
                        .any(|attempt| attempt.status == "cancelled"))
        }
    };
    if needs_finalization {
        envelope.cancel(Utc::now(), Some("process_interrupted"));
    }
    let mut recovered = serde_json::to_vec(&envelope).map_err(LedgerError::Json)?;
    let max = usize::try_from(entry_max_bytes).map_err(|_| LedgerError::InvalidCapacity)?;
    if recovered.len() > max {
        return Err(LedgerError::RecordTooLarge);
    }
    recovered.resize(max, 0);
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(LedgerError::Io)?;
    file.write_all(&recovered).map_err(LedgerError::Io)?;
    file.sync_all().map_err(LedgerError::Io)?;
    drop(file);
    fs::rename(temporary, path).map_err(LedgerError::Io)
}

fn default_operation() -> String {
    "chat_completions".to_owned()
}

fn remove_if_present(path: &Path) -> Result<(), LedgerError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LedgerError::Io(error)),
    }
}

fn first_u64(value: &Value, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_u64))
        .unwrap_or_default()
}

fn value_at(value: &Value, path: &[&str]) -> u64 {
    path.iter()
        .try_fold(value, |current, key| current.get(key))
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

fn bounded(value: Option<String>) -> Option<String> {
    value.map(bounded_text)
}

fn bounded_text(value: String) -> String {
    if value.len() <= 512 {
        return value;
    }
    let mut boundary = 512;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

fn nonempty_bounded(value: String) -> String {
    let value = bounded_text(value.trim().to_owned());
    if value.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        value
    }
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use sqlx::{Pool, Postgres, Row as _};

    use super::*;
    use crate::db;

    #[test]
    fn spool_reserves_real_space_and_rejects_the_next_request() {
        let directory = test_directory("capacity");
        let entry_bytes = MIN_ENTRY_MAX_BYTES;
        let spool = Spool::open(directory.clone(), entry_bytes * 3, entry_bytes).unwrap();
        let first = RequestEnvelope::started(request_start("first"));
        spool.reserve(first.request.id, &first).unwrap();

        let reserved = spool.path(first.request.id, "reserved");
        assert_eq!(fs::metadata(reserved).unwrap().len(), entry_bytes);
        let working = spool.path(first.request.id, "working");
        assert_eq!(fs::metadata(&working).unwrap().len(), entry_bytes);
        let next = spool.path(first.request.id, "next");
        assert_eq!(fs::metadata(&next).unwrap().len(), entry_bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            assert!(fs::metadata(&working).unwrap().blocks() * 512 >= entry_bytes);
        }
        let second = RequestEnvelope::started(request_start("second"));
        assert!(matches!(
            spool.reserve(second.request.id, &second),
            Err(LedgerError::SpoolFull)
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn initial_reservation_durably_contains_the_routing_reason() {
        let directory = test_directory("initial-routing");
        let spool = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let mut start = request_start("routing-before-provider");
        start.policy = Some(
            RequestPolicySnapshot {
                deadline_at: None,
                max_queue_time_ms: None,
                decision: serde_json::json!({"allowed": true}),
            }
            .with_routing("health"),
        );
        let envelope = RequestEnvelope::started(start);
        spool.reserve(envelope.request.id, &envelope).unwrap();

        let bytes = fs::read(spool.path(envelope.request.id, "reserved")).unwrap();
        let end = bytes
            .iter()
            .rposition(|byte| *byte != 0)
            .map_or(0, |index| index + 1);
        let stored: RequestEnvelope = serde_json::from_slice(&bytes[..end]).unwrap();
        assert_eq!(
            stored.request.policy_decision.unwrap()["routing"]["reason"],
            "health"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_recovers_each_side_of_the_completion_rename() {
        let interrupted_directory = test_directory("interrupted-workspace");
        let interrupted = Spool::open(
            interrupted_directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let first = RequestEnvelope::started(request_start("interrupted"));
        interrupted.reserve(first.request.id, &first).unwrap();
        fs::write(interrupted.path(first.request.id, "working"), b"partial").unwrap();
        drop(interrupted);

        let reopened = Spool::open(
            interrupted_directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let recovered = reopened.ready().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].1.request.status, "cancelled");
        assert!(recovered[0].1.request.completed_at.is_some());
        assert!(!reopened.path(first.request.id, "working").exists());
        fs::remove_dir_all(interrupted_directory).unwrap();

        let completed_directory = test_directory("completed-before-cleanup");
        let completed = Spool::open(
            completed_directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let mut second = RequestEnvelope::started(request_start("completed"));
        completed.reserve(second.request.id, &second).unwrap();
        second.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: Some("company-default".to_owned()),
            selected_provider: Some("test-provider".to_owned()),
            attempts: Vec::new(),
            completed_at: Utc::now(),
        });
        second.finalized = Some(true);
        let mut bytes = completed.encode(&second).unwrap();
        bytes.resize(MIN_ENTRY_MAX_BYTES as usize, 0);
        fs::write(completed.path(second.request.id, "working"), bytes).unwrap();
        fs::rename(
            completed.path(second.request.id, "working"),
            completed.path(second.request.id, "ready"),
        )
        .unwrap();
        drop(completed);

        let reopened = Spool::open(
            completed_directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let recovered = reopened.ready().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].1.request.status, "completed");
        assert!(!reopened.path(second.request.id, "reserved").exists());
        fs::remove_dir_all(completed_directory).unwrap();
    }

    #[test]
    fn initial_reservation_durably_contains_the_registry_selection() {
        let directory = test_directory("initial-selection");
        let spool = Spool::open(directory.clone(), 96 * 1024, 32 * 1024).unwrap();
        let model_id = Uuid::new_v4();
        let provider_id = Uuid::new_v4();
        let mut start = request_start("durable-selection");
        start.selection = Some(ModelSelectionSnapshot {
            selector_kind: "quality".to_owned(),
            selector_value: "premium".to_owned(),
            alias_id: Some(Uuid::new_v4()),
            alias_version: Some(2),
            model_id,
            model_version: 3,
            provider_id,
            provider_version: 4,
            provider_model_id: "provider-model".to_owned(),
            cost_metadata: serde_json::json!({"input_microusd": 2}),
        });
        let envelope = RequestEnvelope::started(start);
        spool.reserve(envelope.request.id, &envelope).unwrap();
        drop(spool);

        let reopened = Spool::open(directory.clone(), 96 * 1024, 32 * 1024).unwrap();
        let ready = reopened.ready().unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].1.request.selected_model_id, Some(model_id));
        assert_eq!(ready[0].1.request.selected_provider_id, Some(provider_id));
        assert_eq!(
            ready[0].1.request.selected_provider_model_id.as_deref(),
            Some("provider-model")
        );
        assert_eq!(
            ready[0].1.request.selected_cost_metadata.as_ref().unwrap()["input_microusd"],
            2
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_refinalizes_a_legacy_provisional_stream_checkpoint_conservatively() {
        let directory = test_directory("stream-checkpoint");
        let spool = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let mut envelope = RequestEnvelope::started(request_start("cancelled-stream"));
        envelope.request.quota_reservation_id = Some(Uuid::new_v4());
        let id = envelope.request.id;
        spool.reserve(id, &envelope).unwrap();
        let now = Utc::now();
        let mut attempt = ProviderAttempt::begin(
            "test-provider".to_owned(),
            "claude-test".to_owned(),
            Some("provider-request".to_owned()),
            now,
            TokenUsage {
                input_tokens: 11,
                ..TokenUsage::default()
            },
        );
        attempt.observe_first_byte(now);
        attempt.observe_response_id(Some("msg_checkpoint".to_owned()));
        attempt.observe_usage(
            TokenUsage {
                input_tokens: 10,
                output_tokens: 2,
                ..TokenUsage::default()
            },
            now,
        );
        envelope.complete(RequestCompletion {
            status: "cancelled".to_owned(),
            selected_model: Some("claude-test".to_owned()),
            selected_provider: Some("test-provider".to_owned()),
            attempts: vec![attempt],
            completed_at: now,
        });
        envelope.finalized = None;
        spool.checkpoint(id, &envelope).unwrap();
        drop(spool);

        let reopened = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let recovered = reopened.ready().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].1.request.status, "cancelled");
        assert_eq!(recovered[0].1.finalized, Some(true));
        assert_eq!(recovered[0].1.request.quota_actual_tokens, None);
        assert_eq!(recovered[0].1.request.quota_actual_cost_microusd, None);
        assert_eq!(
            recovered[0].1.attempts[0].provider_response_id.as_deref(),
            Some("msg_checkpoint")
        );
        assert_eq!(
            recovered[0].1.attempts[0].error_type.as_deref(),
            Some("process_interrupted")
        );
        assert_eq!(recovered[0].1.usage_records.len(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_preserves_a_final_legacy_envelope_without_the_marker() {
        let directory = test_directory("legacy-final-marker");
        let spool = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let mut envelope = RequestEnvelope::started(request_start("legacy-final-marker"));
        let id = envelope.request.id;
        spool.reserve(id, &envelope).unwrap();
        envelope.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: Some("claude-test".to_owned()),
            selected_provider: Some("test-provider".to_owned()),
            attempts: Vec::new(),
            completed_at: Utc::now(),
        });
        envelope.finalized = None;
        spool.checkpoint(id, &envelope).unwrap();
        spool.publish(id).unwrap();
        drop(spool);

        let reopened = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let recovered = reopened.ready().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].1.request.status, "completed");
        assert_eq!(recovered[0].1.finalized, None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn final_checkpoint_survives_a_publish_failure() {
        let directory = test_directory("publish-failure");
        let dead_pool = db::pool("postgres://gateway@127.0.0.1:1/gateway", 1).unwrap();
        let ledger = Ledger::open(
            dead_pool,
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let reservation = ledger
            .reserve(request_start("publish-failure"))
            .await
            .unwrap();
        let id = reservation.id.unwrap();
        let ready_obstacle = ledger.inner.spool.path(id, "ready");
        fs::create_dir(&ready_obstacle).unwrap();

        let result = reservation
            .finish(RequestCompletion {
                status: "completed".to_owned(),
                selected_model: Some("company-default".to_owned()),
                selected_provider: Some("test-provider".to_owned()),
                attempts: Vec::new(),
                completed_at: Utc::now(),
            })
            .await;
        assert!(matches!(result, Err(LedgerError::Io(_))));
        fs::remove_dir(&ready_obstacle).unwrap();
        drop(ledger);

        let reopened = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let recovered = reopened.ready().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].1.request.status, "completed");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_torn_later_checkpoint_preserves_the_previous_complete_one() {
        let directory = test_directory("torn-later-checkpoint");
        let spool = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let mut envelope = RequestEnvelope::started(request_start("torn-checkpoint"));
        let id = envelope.request.id;
        spool.reserve(id, &envelope).unwrap();
        envelope.complete(RequestCompletion {
            status: "cancelled".to_owned(),
            selected_model: Some("claude-test".to_owned()),
            selected_provider: Some("test-provider".to_owned()),
            attempts: Vec::new(),
            completed_at: Utc::now(),
        });
        spool.checkpoint(id, &envelope).unwrap();

        fs::write(spool.path(id, "next"), b"partial replacement").unwrap();
        drop(spool);

        let reopened = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let recovered = reopened.ready().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].1.request.status, "cancelled");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cancellation_publish_waits_for_an_inflight_checkpoint() {
        let directory = test_directory("checkpoint-race");
        let spool = Arc::new(
            Spool::open(
                directory.clone(),
                MIN_ENTRY_MAX_BYTES * 3,
                MIN_ENTRY_MAX_BYTES,
            )
            .unwrap(),
        );
        let mut envelope = RequestEnvelope::started(request_start("racing-stream"));
        let id = envelope.request.id;
        spool.reserve(id, &envelope).unwrap();
        envelope.complete(RequestCompletion {
            status: "cancelled".to_owned(),
            selected_model: Some("claude-test".to_owned()),
            selected_provider: Some("test-provider".to_owned()),
            attempts: Vec::new(),
            completed_at: Utc::now(),
        });
        spool.begin_checkpoint(id).unwrap();

        let (sent, received) = std::sync::mpsc::channel();
        let publisher = Arc::clone(&spool);
        let publish = envelope.clone();
        let task = std::thread::spawn(move || sent.send(publisher.finish(id, &publish)).unwrap());
        assert!(
            received
                .recv_timeout(std::time::Duration::from_millis(25))
                .is_err(),
            "cancellation published before the checkpoint completed"
        );
        spool.checkpoint(id, &envelope).unwrap();
        spool.end_checkpoint(id);
        received
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap()
            .unwrap();
        task.join().unwrap();

        let recovered = spool.ready().unwrap();
        assert_eq!(recovered[0].1.request.status, "cancelled");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_cancels_a_pre_provider_reservation_and_reconciles_quota_to_zero() {
        let directory = test_directory("quota-recovery");
        let spool = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let mut envelope = RequestEnvelope::started(request_start("quota-recovery"));
        envelope.request.quota_reservation_id = Some(Uuid::new_v4());
        let id = envelope.request.id;
        spool.reserve(id, &envelope).unwrap();
        drop(spool);

        let reopened = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let recovered = reopened.ready().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].1.request.status, "cancelled");
        assert!(recovered[0].1.request.completed_at.is_some());
        assert_eq!(recovered[0].1.request.quota_actual_tokens, Some(0));
        assert_eq!(recovered[0].1.request.quota_actual_cost_microusd, Some(0));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_refinalizes_an_incomplete_ready_envelope_after_a_second_crash() {
        let directory = test_directory("quota-ready-recovery");
        let spool = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let mut envelope = RequestEnvelope::started(request_start("quota-ready-recovery"));
        envelope.request.quota_reservation_id = Some(Uuid::new_v4());
        let id = envelope.request.id;
        spool.reserve(id, &envelope).unwrap();
        fs::rename(spool.path(id, "reserved"), spool.path(id, "ready")).unwrap();
        drop(spool);

        let reopened = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let recovered = reopened.ready().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].1.request.status, "cancelled");
        assert!(recovered[0].1.request.completed_at.is_some());
        assert_eq!(recovered[0].1.request.quota_actual_tokens, Some(0));
        assert_eq!(recovered[0].1.request.quota_actual_cost_microusd, Some(0));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn spool_decodes_records_written_before_stream_fields_existed() {
        let directory = test_directory("old-spool");
        fs::create_dir_all(&directory).unwrap();
        let envelope = RequestEnvelope::started(request_start("old-record"));
        let id = envelope.request.id;
        let mut value = serde_json::to_value(envelope).unwrap();
        let request = value["request"].as_object_mut().unwrap();
        for field in [
            "operation",
            "client_agent_id",
            "parent_agent_id",
            "request_received_at",
            "queue_entered_at",
            "queue_left_at",
            "client_first_byte_at",
            "total_latency_ms",
            "gateway_overhead_ms",
        ] {
            request.remove(field);
        }
        request.insert("queue_duration_ms".to_owned(), Value::from(0));
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.resize(MIN_ENTRY_MAX_BYTES as usize, 0);
        fs::write(directory.join(format!("{}.reserved", id.simple())), bytes).unwrap();
        fs::write(
            directory.join(format!("{}.working", id.simple())),
            vec![0; MIN_ENTRY_MAX_BYTES as usize],
        )
        .unwrap();

        let reopened = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let recovered = reopened.ready().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].1.request.operation, "chat_completions");
        assert_eq!(recovered[0].1.request.queue_duration_ms, Some(0));
        assert_eq!(recovered[0].1.request.request_received_at, None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[sqlx::test]
    async fn failed_database_flush_stays_durable_and_recovers_once(pool: Pool<Postgres>) {
        let directory = test_directory("recovery");
        let spool = Spool::open(directory.clone(), 192 * 1024, 32 * 1024).unwrap();
        let mut envelope = RequestEnvelope::started(request_start("recovery-request"));
        let request_id = envelope.request.id;
        spool.reserve(request_id, &envelope).unwrap();
        let now = Utc::now();
        envelope.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: Some("company-default".to_owned()),
            selected_provider: Some("test-provider".to_owned()),
            attempts: vec![ProviderAttempt::completed(
                CompletedAttempt {
                    provider: "test-provider".to_owned(),
                    model: "company-default".to_owned(),
                    provider_request_id: Some("provider-request".to_owned()),
                    provider_response_id: Some("provider-response".to_owned()),
                    started_at: now,
                    completed_at: now,
                    status: "completed".to_owned(),
                    error_type: None,
                },
                br#"{"prompt":"sentinel-prompt","usage":{"prompt_tokens":10,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":4},"cache_creation_input_tokens":2,"output_tokens_details":{"reasoning_tokens":1}}}"#,
            )],
            completed_at: now,
        });
        spool.finish(request_id, &envelope).unwrap();
        let ready = fs::read_to_string(spool.path(request_id, "ready")).unwrap();
        assert!(!ready.contains("sentinel-prompt"));

        let dead_pool = db::pool("postgres://gateway@127.0.0.1:1/gateway", 1).unwrap();
        let unavailable =
            Ledger::open(dead_pool, directory.clone(), 192 * 1024, 32 * 1024).unwrap();
        unavailable.schedule_flush(&tokio::runtime::Handle::current());
        unavailable.wait_for_idle().await;
        assert_eq!(ready_count(&directory), 1);
        assert!(
            !unavailable
                .inner
                .database_writes_enabled
                .load(Ordering::Acquire)
        );

        let mut second = RequestEnvelope::started(request_start("second-after-outage"));
        let second_id = second.request.id;
        unavailable.inner.spool.reserve(second_id, &second).unwrap();
        second.complete(RequestCompletion {
            status: "failed".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: Vec::new(),
            completed_at: Utc::now(),
        });
        unavailable.inner.spool.finish(second_id, &second).unwrap();
        unavailable.schedule_flush(&tokio::runtime::Handle::current());
        unavailable.wait_for_idle().await;
        assert_eq!(ready_count(&directory), 2);
        assert!(
            !unavailable
                .inner
                .database_writes_enabled
                .load(Ordering::Acquire)
        );

        let recovered =
            Ledger::open(pool.clone(), directory.clone(), 192 * 1024, 32 * 1024).unwrap();
        assert_eq!(recovered.flush_startup().await.unwrap(), 2);
        assert_eq!(ready_count(&directory), 0);

        let row = sqlx::query(
            r#"
            SELECT
                (SELECT count(*) FROM gateway_requests) AS requests,
                (SELECT count(*) FROM provider_attempts) AS attempts,
                (SELECT count(*) FROM usage_records) AS usage,
                (SELECT cached_input_tokens FROM usage_records) AS cached,
                (SELECT cache_write_input_tokens FROM usage_records) AS cache_write,
                (SELECT reasoning_tokens FROM usage_records) AS reasoning
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<i64, _>("requests"), 2);
        assert_eq!(row.get::<i64, _>("attempts"), 1);
        assert_eq!(row.get::<i64, _>("usage"), 1);
        assert_eq!(row.get::<i64, _>("cached"), 4);
        assert_eq!(row.get::<i64, _>("cache_write"), 2);
        assert_eq!(row.get::<i64, _>("reasoning"), 1);

        recovered
            .inner
            .spool
            .reserve(request_id, &envelope)
            .unwrap();
        recovered.inner.spool.finish(request_id, &envelope).unwrap();
        assert_eq!(recovered.flush_startup().await.unwrap(), 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM usage_records")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1,
            "replaying the same envelope duplicated usage"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[sqlx::test]
    async fn a_pending_completion_is_included_after_the_flush_gate_opens(pool: Pool<Postgres>) {
        let directory = test_directory("pending-batch");
        let ledger = Ledger::open(
            pool.clone(),
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 6,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let held_gate = Arc::clone(&ledger.inner.flush_gate)
            .acquire_owned()
            .await
            .unwrap();

        let first = RequestEnvelope::started(request_start("first-pending"));
        ledger
            .inner
            .spool
            .reserve(first.request.id, &first)
            .unwrap();
        ledger.inner.spool.finish(first.request.id, &first).unwrap();
        ledger.schedule_flush(&tokio::runtime::Handle::current());

        let second = RequestEnvelope::started(request_start("second-pending"));
        ledger
            .inner
            .spool
            .reserve(second.request.id, &second)
            .unwrap();
        ledger
            .inner
            .spool
            .finish(second.request.id, &second)
            .unwrap();
        ledger.schedule_flush(&tokio::runtime::Handle::current());

        drop(held_gate);
        ledger.wait_for_idle().await;
        assert_eq!(ready_count(&directory), 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM gateway_requests")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_minimum_slot_fits_the_largest_current_single_attempt_envelope() {
        let directory = test_directory("minimum-envelope");
        assert!(matches!(
            Spool::open(
                directory.clone(),
                MIN_ENTRY_MAX_BYTES,
                MIN_ENTRY_MAX_BYTES / 2
            ),
            Err(LedgerError::InvalidCapacity)
        ));

        let spool = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let long = "x".repeat(2048);
        let bounded_registry_id = "m".repeat(255);
        let bounded_cost_metadata = serde_json::json!({
            "padding": "c".repeat(4082)
        });
        assert!(serde_json::to_vec(&bounded_cost_metadata).unwrap().len() <= 4096);
        let mut envelope = RequestEnvelope::started(RequestStart {
            request_id: long.clone(),
            session_id: Some(long.clone()),
            trace_id: Some(long.clone()),
            requested_model: Some(long.clone()),
            workload_class: Some(long.clone()),
            urgency: Some(long.clone()),
            quality: Some(long.clone()),
            budget_class: Some(long.clone()),
            estimated_tokens: Some(i64::MAX),
            selection: Some(ModelSelectionSnapshot {
                selector_kind: "quality".to_owned(),
                selector_value: "premium".to_owned(),
                alias_id: Some(Uuid::new_v4()),
                alias_version: Some(i64::MAX),
                model_id: Uuid::new_v4(),
                model_version: i64::MAX,
                provider_id: Uuid::new_v4(),
                provider_version: i64::MAX,
                provider_model_id: bounded_registry_id,
                cost_metadata: bounded_cost_metadata,
            }),
            policy: None,
            principal: PrincipalContext::bootstrap(),
            created_at: Utc::now(),
        });
        let now = Utc::now();
        envelope.complete(RequestCompletion {
            status: long.clone(),
            selected_model: Some(long.clone()),
            selected_provider: Some(long.clone()),
            attempts: vec![ProviderAttempt::completed(
                CompletedAttempt {
                    provider: long.clone(),
                    model: long.clone(),
                    provider_request_id: Some(long.clone()),
                    provider_response_id: Some(long.clone()),
                    started_at: now,
                    completed_at: now,
                    status: long.clone(),
                    error_type: Some(long),
                },
                br#"{"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
            )],
            completed_at: now,
        });
        assert!(spool.encode(&envelope).unwrap().len() < MIN_ENTRY_MAX_BYTES as usize);
        fs::remove_dir_all(directory).unwrap();
    }

    fn request_start(request_id: &str) -> RequestStart {
        RequestStart {
            request_id: request_id.to_owned(),
            session_id: Some("session".to_owned()),
            trace_id: Some("trace".to_owned()),
            requested_model: Some("company-default".to_owned()),
            workload_class: None,
            urgency: None,
            quality: None,
            budget_class: None,
            estimated_tokens: None,
            selection: None,
            policy: None,
            principal: PrincipalContext::bootstrap(),
            created_at: Utc::now(),
        }
    }

    fn ready_count(directory: &Path) -> usize {
        fs::read_dir(directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("ready")
            })
            .count()
    }

    #[cfg(unix)]
    #[test]
    fn a_spool_directory_the_process_cannot_write_is_refused_at_open() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = test_directory("readonly");
        fs::create_dir_all(&directory).unwrap();
        let mut mode = fs::metadata(&directory).unwrap().permissions();
        mode.set_mode(0o555);
        fs::set_permissions(&directory, mode).unwrap();
        // root ignores the mode bits, so state which case this run exercises
        // instead of passing for the wrong reason.
        let writable = fs::write(directory.join("who.tmp"), b"").is_ok();

        let opened = Spool::open(directory.clone(), 1024 * 1024, MIN_ENTRY_MAX_BYTES);

        let mut mode = fs::metadata(&directory).unwrap().permissions();
        mode.set_mode(0o755);
        fs::set_permissions(&directory, mode).unwrap();
        let left_behind: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        fs::remove_dir_all(&directory).unwrap();

        if writable {
            opened.unwrap();
            assert_eq!(left_behind, ["who.tmp"], "the probe file was left behind");
        } else {
            assert!(
                matches!(opened, Err(LedgerError::Io(_))),
                "an unwritable spool must not open"
            );
            assert!(left_behind.is_empty());
        }
    }

    fn test_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "agent-pool-gateway-ledger-{name}-{}",
            Uuid::new_v4()
        ))
    }
}
