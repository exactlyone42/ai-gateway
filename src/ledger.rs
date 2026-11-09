use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Pool, Postgres};
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
const FLUSH_BATCH_SIZE: usize = 32;
pub const MIN_ENTRY_MAX_BYTES: u64 = 32 * 1024;

struct FlushBatch {
    flushed: usize,
    more: bool,
}

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("ledger spool configuration is invalid")]
    InvalidCapacity,
    #[error("ledger spool is full")]
    SpoolFull,
    #[error("ledger spool is already in use")]
    SpoolBusy,
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
    #[error("ledger checkpoint is already active")]
    CheckpointBusy,
    #[error("ledger backup coordination busy")]
    BackupBusy,
    #[error("ledger backup coordination unavailable")]
    BackupUnavailable,
    #[error("usage database is unavailable")]
    Database(#[source] sqlx::Error),
}

#[derive(Clone)]
pub struct Ledger {
    inner: Arc<LedgerInner>,
}

struct PendingFinalization {
    ledger: Ledger,
}

impl PendingFinalization {
    fn new(ledger: Ledger) -> Self {
        ledger
            .inner
            .finalizations_pending
            .fetch_add(1, Ordering::AcqRel);
        Self { ledger }
    }
}

impl Drop for PendingFinalization {
    fn drop(&mut self) {
        if self
            .ledger
            .inner
            .finalizations_pending
            .fetch_sub(1, Ordering::AcqRel)
            == 1
        {
            self.ledger.inner.finalizations_idle.notify_waiters();
        }
    }
}

struct LedgerInner {
    pool: Pool<Postgres>,
    spool: Arc<Spool>,
    capture_backup_root: Option<PathBuf>,
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

    pub fn with_task_routing(mut self, task_routing: Value) -> Self {
        if let Value::Object(decision) = &mut self.decision
            && let Some(Value::Object(routing)) = decision.get_mut("routing")
        {
            routing.insert("task_classification".to_owned(), task_routing);
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
    backup_guard: Option<Arc<crate::capture_backup_barrier::CaptureBarrierLease>>,
    id: Option<Uuid>,
    envelope: RequestEnvelope,
    _scheduler_lease: Option<SchedulerLease>,
    routing_lease: Option<RoutingLease>,
    force_token: Option<CancellationToken>,
    capture: Option<Arc<crate::capture_session::AsyncCaptureSession>>,
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
    #[serde(default)]
    requested_parameters: Option<Value>,
    #[serde(default)]
    provider_parameters: Option<Value>,
    #[serde(default)]
    attribution: Option<RequestAttribution>,
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

#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct RequestAttribution {
    prompt_id: Option<String>,
    prompt_version: Option<String>,
    skill_set_id: Option<String>,
    agent_name: Option<String>,
    agent_version: Option<String>,
    client_version: Option<String>,
    task_id: Option<String>,
}

impl RequestAttribution {
    pub(crate) fn from_headers(headers: &axum::http::HeaderMap) -> Self {
        fn value(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
            let mut values = headers.get_all(name).iter();
            let bytes = values.next()?.as_bytes();
            if values.next().is_some()
                || bytes.is_empty()
                || bytes.len() > 128
                || !bytes.iter().all(|byte| (0x20..=0x7e).contains(byte))
            {
                return None;
            }
            String::from_utf8(bytes.to_vec()).ok()
        }
        Self {
            prompt_id: value(headers, "x-apg-prompt-id"),
            prompt_version: value(headers, "x-apg-prompt-version"),
            skill_set_id: value(headers, "x-apg-skill-set-id"),
            agent_name: value(headers, "x-apg-agent-name"),
            agent_version: value(headers, "x-apg-agent-version"),
            client_version: value(headers, "x-apg-client-version"),
            task_id: value(headers, "x-apg-task-id"),
        }
    }
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
    _instance_lock: File,
    entry_max_bytes: u64,
    state: Mutex<SpoolState>,
    checkpointing: Mutex<HashSet<Uuid>>,
    checkpoint_idle: Condvar,
    checkpoint_async_idle: Notify,
}

impl Drop for Spool {
    fn drop(&mut self) {
        if let Err(error) = self._instance_lock.unlock() {
            tracing::error!(error = %error, error.type = "ledger_spool_unlock_failed", "ledger spool lock release failed");
        }
    }
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
        Self::open_coordinated(pool, directory, max_bytes, entry_max_bytes, None)
    }

    pub(crate) fn open_coordinated(
        pool: Pool<Postgres>,
        directory: PathBuf,
        max_bytes: u64,
        entry_max_bytes: u64,
        capture_backup_root: Option<PathBuf>,
    ) -> Result<Self, LedgerError> {
        fs::create_dir_all(&directory).map_err(LedgerError::Io)?;
        let _backup_guard = capture_backup_root
            .as_ref()
            .map(|root| {
                crate::capture_backup_barrier::try_shared(root).map_err(|error| match error {
                    crate::capture_backup_barrier::CaptureBarrierError::Busy => {
                        LedgerError::BackupBusy
                    }
                    crate::capture_backup_barrier::CaptureBarrierError::Unavailable => {
                        LedgerError::BackupUnavailable
                    }
                })
            })
            .transpose()?;
        let spool = Arc::new(Spool::open(directory, max_bytes, entry_max_bytes)?);
        Ok(Self {
            inner: Arc::new(LedgerInner {
                pool,
                spool,
                capture_backup_root,
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
        self.reserve_with_quota(start, None, None, None).await
    }

    pub(crate) async fn reserve_with_quota(
        &self,
        start: RequestStart,
        gateway_request_id: Option<Uuid>,
        quota: Option<RequestQuotaSnapshot>,
        attribution: Option<&RequestAttribution>,
    ) -> Result<Reservation, LedgerError> {
        let backup_guard = if let Some(root) = self.inner.capture_backup_root.clone() {
            Some(Arc::new(
                tokio::task::spawn_blocking(move || {
                    crate::capture_backup_barrier::try_shared(&root).map_err(|error| match error {
                        crate::capture_backup_barrier::CaptureBarrierError::Busy => {
                            LedgerError::BackupBusy
                        }
                        crate::capture_backup_barrier::CaptureBarrierError::Unavailable => {
                            LedgerError::BackupUnavailable
                        }
                    })
                })
                .await
                .map_err(LedgerError::Worker)??,
            ))
        } else {
            None
        };
        let mut envelope = RequestEnvelope::started_with(start, gateway_request_id, quota);
        envelope.request.attribution = attribution.cloned();
        let id = envelope.request.id;
        let spool = Arc::clone(&self.inner.spool);
        let ledger = self.clone();
        let pending = PendingFinalization::new(self.clone());
        let (reservation, pending) = tokio::task::spawn_blocking(move || {
            spool.reserve(id, &envelope)?;
            Ok((
                Reservation {
                    ledger,
                    backup_guard,
                    id: Some(id),
                    envelope,
                    _scheduler_lease: None,
                    routing_lease: None,
                    force_token: None,
                    capture: None,
                },
                pending,
            ))
        })
        .await
        .map_err(LedgerError::Worker)??;
        drop(pending);
        Ok(reservation)
    }

    pub(crate) async fn reject(
        &self,
        start: RequestStart,
        operation: &str,
        client_agent_id: Option<String>,
        parent_agent_id: Option<String>,
        attribution: &RequestAttribution,
    ) -> Result<(), LedgerError> {
        self.reject_with_quota(
            start,
            operation,
            client_agent_id,
            parent_agent_id,
            None,
            attribution,
        )
        .await
    }

    pub(crate) async fn reject_with_quota(
        &self,
        start: RequestStart,
        operation: &str,
        client_agent_id: Option<String>,
        parent_agent_id: Option<String>,
        quota: Option<RequestQuotaSnapshot>,
        attribution: &RequestAttribution,
    ) -> Result<(), LedgerError> {
        let mut reservation = self
            .reserve_with_quota(start, None, quota, Some(attribution))
            .await?;
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
        let mut total = 0;
        loop {
            let batch = self.flush_once().await?;
            total += batch.flushed;
            if !batch.more {
                return Ok(total);
            }
            tokio::task::yield_now().await;
        }
    }

    pub(crate) async fn wait_for_idle(&self) {
        loop {
            let notified = self.inner.flush_idle.notified();
            if !self.inner.flush_worker_scheduled.load(Ordering::Acquire)
                && (!self.inner.flush_pending.load(Ordering::Acquire)
                    || !self.inner.database_writes_enabled.load(Ordering::Acquire))
            {
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

    async fn flush_once(&self) -> Result<FlushBatch, LedgerError> {
        let backup_guard = if let Some(root) = self.inner.capture_backup_root.clone() {
            Some(Arc::new(
                tokio::task::spawn_blocking(move || {
                    crate::capture_backup_barrier::try_shared(&root).map_err(|error| match error {
                        crate::capture_backup_barrier::CaptureBarrierError::Busy => {
                            LedgerError::BackupBusy
                        }
                        crate::capture_backup_barrier::CaptureBarrierError::Unavailable => {
                            LedgerError::BackupUnavailable
                        }
                    })
                })
                .await
                .map_err(LedgerError::Worker)??,
            ))
        } else {
            None
        };
        let spool = Arc::clone(&self.inner.spool);
        let ready_guard = backup_guard.clone();
        let (mut ready, retained_reservations) = tokio::task::spawn_blocking(move || {
            let _guard = ready_guard;
            let ready = spool.ready()?;
            let retained = if ready.is_empty() {
                HashSet::new()
            } else {
                spool.conflict_reservations()?
            };
            Ok::<_, LedgerError>((ready, retained))
        })
        .await
        .map_err(LedgerError::Worker)??;
        if ready.is_empty() {
            return Ok(FlushBatch {
                flushed: 0,
                more: false,
            });
        }
        let mut reservations = HashMap::new();
        for (_, envelope) in &ready {
            if let Some(reservation) = envelope.request.quota_reservation_id {
                *reservations
                    .entry((envelope.request.tenant_id, reservation))
                    .or_insert(0usize) += 1;
            }
        }
        let mut ambiguous = Vec::new();
        ready.retain(|(path, envelope)| {
            let duplicate = envelope
                .request
                .quota_reservation_id
                .is_some_and(|reservation| {
                    reservations
                        .get(&(envelope.request.tenant_id, reservation))
                        .copied()
                        .unwrap_or(0)
                        > 1
                        || retained_reservations
                            .contains(&(envelope.request.tenant_id, reservation))
                });
            if duplicate {
                ambiguous.push(path.clone());
            }
            !duplicate
        });
        if !ambiguous.is_empty() {
            let spool = Arc::clone(&self.inner.spool);
            let guard = backup_guard.clone();
            tokio::task::spawn_blocking(move || {
                let _guard = guard;
                for path in &ambiguous {
                    spool.quarantine(path)?;
                }
                Ok::<_, LedgerError>(())
            })
            .await
            .map_err(LedgerError::Worker)??;
            tracing::error!(error.type = "ledger_conflicting_reservation", "conflicting usage records retained for review");
        }
        let more = ready.len() > FLUSH_BATCH_SIZE;
        ready.truncate(FLUSH_BATCH_SIZE);
        let mut pending = vec![ready];
        let mut flushed = 0;
        while let Some(batch) = pending.pop() {
            if batch.is_empty() {
                continue;
            }
            let payload =
                serde_json::to_value(batch.iter().map(|entry| &entry.1).collect::<Vec<_>>())
                    .map_err(LedgerError::Json)?;
            match sqlx::query_file!("queries/ingest_usage_envelopes.sql", payload)
                .execute(&self.inner.pool)
                .await
            {
                Ok(_) => {
                    flushed += batch.len();
                    let paths = batch.into_iter().map(|entry| entry.0).collect::<Vec<_>>();
                    let spool = Arc::clone(&self.inner.spool);
                    let guard = backup_guard.clone();
                    tokio::task::spawn_blocking(move || {
                        let _guard = guard;
                        spool.remove(&paths)
                    })
                    .await
                    .map_err(LedgerError::Worker)??;
                }
                Err(error)
                    if error
                        .as_database_error()
                        .and_then(|value| value.code())
                        .as_deref()
                        == Some("GW409") =>
                {
                    if batch.len() > 1 {
                        let mut first = batch;
                        let second = first.split_off(first.len() / 2);
                        pending.push(second);
                        pending.push(first);
                    } else {
                        let path = batch[0].0.clone();
                        let spool = Arc::clone(&self.inner.spool);
                        let guard = backup_guard.clone();
                        tokio::task::spawn_blocking(move || {
                            let _guard = guard;
                            spool.quarantine(&path)
                        })
                        .await
                        .map_err(LedgerError::Worker)??;
                        tracing::error!(error.type = "ledger_conflicting_record", "conflicting usage record retained for review");
                    }
                }
                Err(error) => return Err(LedgerError::Database(error)),
            }
        }
        Ok(FlushBatch { flushed, more })
    }

    fn schedule_flush(&self, runtime: &tokio::runtime::Handle) {
        if !self.inner.database_writes_enabled.load(Ordering::Acquire) {
            return;
        }
        self.inner.flush_pending.store(true, Ordering::Release);
        std::sync::atomic::fence(Ordering::SeqCst);
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
            let mut deferred = false;
            let permit = Arc::clone(&ledger.inner.flush_gate).acquire_owned().await;
            if let Ok(permit) = permit {
                let should_flush = ledger.inner.flush_pending.swap(false, Ordering::AcqRel)
                    && ledger.inner.database_writes_enabled.load(Ordering::Acquire);
                if should_flush {
                    match ledger.flush_once().await {
                    Ok(batch) if batch.more => { ledger.inner.flush_pending.store(true, Ordering::Release); }
                    Ok(_) => {}
                    Err(LedgerError::BackupBusy) => {
                        ledger.inner.flush_pending.store(true, Ordering::Release);
                        deferred = true;
                    }
                    Err(error) => {
                        ledger
                            .inner
                            .database_writes_enabled
                            .store(false, Ordering::Release);
                        tracing::error!(
                            error.type = "ledger_flush_failed",
                            exception.message = %error,
                            "usage remains in the durable spool; resolve the reported error before restart"
                        );
                    }
                    }
                }
                drop(permit);
            } else {
                tracing::error!(
                    error.type = "ledger_flush_gate_closed",
                    "usage remains in the durable spool"
                );
            }

            if deferred {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }

            ledger
                .inner
                .flush_worker_scheduled
                .store(false, Ordering::Release);
            std::sync::atomic::fence(Ordering::SeqCst);
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
    pub(crate) fn attach_capture(
        &mut self,
        capture: Arc<crate::capture_session::AsyncCaptureSession>,
    ) {
        self.capture = Some(capture);
    }

    pub(crate) fn capture(&self) -> Option<Arc<crate::capture_session::AsyncCaptureSession>> {
        self.capture.clone()
    }
    pub fn annotate_parameters(&mut self, requested: Value, provider: Value) {
        self.envelope.request.requested_parameters = Some(requested);
        self.envelope.request.provider_parameters = Some(provider);
    }

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
        spool.wait_for_checkpoint_async(id).await?;
        let checkpoint = CheckpointGuard::begin(Arc::clone(&spool), id)?;
        let envelope = self.envelope.clone();
        let backup_guard = self.backup_guard.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = backup_guard;
            let _checkpoint = checkpoint;
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
            lease.conclude(completion.completed_at);
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
        let backup_guard = self.backup_guard.clone();
        let runtime = tokio::runtime::Handle::current();
        let pending = PendingFinalization::new(ledger.clone());
        tokio::task::spawn_blocking(move || {
            let _pending = pending;
            let _guard = backup_guard;
            let result = spool.publish(id);
            match &result {
                Ok(()) => ledger.schedule_flush(&runtime),
                Err(error) => tracing::error!(
                    error.type = "ledger_publish_failed",
                    exception.message = %error,
                    exception.os_error = ?match error {
                        LedgerError::Io(source) => source.raw_os_error(),
                        _ => None,
                    },
                    "ledger publish failed; inspect the durable spool"
                ),
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
        let backup_guard = self.backup_guard.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let flush_handle = handle.clone();
                let pending = Arc::new(PendingFinalization::new(ledger.clone()));
                handle.spawn(async move {
                    let result = match ledger.inner.spool.wait_for_checkpoint_async(id).await {
                        Ok(()) => {
                            let spool = ledger.inner.spool.clone();
                            let worker_pending = pending.clone();
                            let worker_guard = backup_guard.clone();
                            tokio::task::spawn_blocking(move || {
                                let _pending = worker_pending;
                                let _guard = worker_guard;
                                spool.finish(id, &envelope)
                            })
                            .await
                            .map_err(LedgerError::Worker)
                            .and_then(|result| result)
                        }
                        Err(error) => Err(error),
                    };
                    match result {
                        Ok(()) => {
                            ledger.schedule_flush(&flush_handle);
                        }
                        Err(error) => tracing::error!(
                            error.type = "ledger_interrupted_request_not_finalized",
                            exception.message = %error,
                            "an interrupted request remains reserved in the spool"
                        ),
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
                requested_parameters: None,
                provider_parameters: None,
                attribution: None,
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

        self.request.gateway_overhead_ms = self.attempts.first().and_then(|attempt| {
            let request_processing = self
                .request
                .request_received_at
                .zip(attempt.provider_request_at)
                .map(|(received, provider)| {
                    (provider - received).num_milliseconds().max(0)
                        - self.request.queue_duration_ms.unwrap_or(0).max(0)
                })?;
            let response_processing = self
                .request
                .client_first_byte_at
                .zip(attempt.provider_first_byte_at)
                .map(|(client, provider)| (client - provider).num_milliseconds().max(0))?;
            Some(request_processing.max(0) + response_processing)
        });

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
        let lock_path = directory.join(".ledger-spool.lock");
        let instance_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&lock_path)
            .map_err(LedgerError::Io)?;
        let metadata = instance_lock.metadata().map_err(LedgerError::Io)?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(LedgerError::InvalidCapacity);
        }
        instance_lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => LedgerError::SpoolBusy,
            std::fs::TryLockError::Error(error) => LedgerError::Io(error),
        })?;
        let current = fs::symlink_metadata(&lock_path).map_err(LedgerError::Io)?;
        if !current.is_file()
            || metadata.nlink() != 1
            || current.dev() != metadata.dev()
            || current.ino() != metadata.ino()
        {
            return Err(LedgerError::InvalidCapacity);
        }
        let mut states: HashMap<String, HashSet<String>> = HashMap::new();
        for entry in fs::read_dir(&directory).map_err(LedgerError::Io)? {
            let path = entry.map_err(LedgerError::Io)?.path();
            let extension = path.extension().and_then(|value| value.to_str());
            match extension {
                Some("tmp") => fs::remove_file(path).map_err(LedgerError::Io)?,
                Some(extension @ ("reserved" | "working" | "next" | "ready" | "conflict")) => {
                    read_bounded(&path, entry_max_bytes)?;
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
            if extensions.contains("conflict") {
                if extensions.len() != 1 {
                    return Err(LedgerError::InvalidCapacity);
                }
                ids.insert(stem);
                continue;
            }
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
                if extensions.contains("working")
                    && self_contained_envelope(&working, entry_max_bytes)?
                {
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
            _instance_lock: instance_lock,
            entry_max_bytes,
            state: Mutex::new(SpoolState {
                slots_used: ids.len(),
                slots_max,
            }),
            checkpointing: Mutex::new(HashSet::new()),
            checkpoint_idle: Condvar::new(),
            checkpoint_async_idle: Notify::new(),
        })
    }

    fn begin_checkpoint(&self, id: Uuid) -> Result<(), LedgerError> {
        let inserted = self
            .checkpointing
            .lock()
            .map_err(|_| LedgerError::InvalidCapacity)?
            .insert(id);
        if inserted {
            Ok(())
        } else {
            Err(LedgerError::CheckpointBusy)
        }
    }

    fn end_checkpoint(&self, id: Uuid) {
        if let Ok(mut active) = self.checkpointing.lock() {
            active.remove(&id);
        }
        self.checkpoint_idle.notify_all();
        self.checkpoint_async_idle.notify_waiters();
    }

    async fn wait_for_checkpoint_async(&self, id: Uuid) -> Result<(), LedgerError> {
        loop {
            let notified = self.checkpoint_async_idle.notified();
            let active = self
                .checkpointing
                .lock()
                .map_err(|_| LedgerError::InvalidCapacity)?
                .contains(&id);
            if !active {
                return Ok(());
            }
            notified.await;
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
        match fs::symlink_metadata(self.path(id, "conflict")) {
            Ok(_) => return Err(LedgerError::InvalidCapacity),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(LedgerError::Io(error)),
        }
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
                let envelope = self.read_envelope(&path, "ready")?;
                Ok((path, envelope))
            })
            .collect()
    }

    fn conflict_reservations(&self) -> Result<HashSet<(Uuid, Uuid)>, LedgerError> {
        let mut reservations = HashSet::new();
        for entry in fs::read_dir(&self.directory).map_err(LedgerError::Io)? {
            let path = entry.map_err(LedgerError::Io)?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("conflict") {
                continue;
            }
            let envelope = self.read_envelope(&path, "conflict")?;
            if let Some(reservation) = envelope.request.quota_reservation_id {
                reservations.insert((envelope.request.tenant_id, reservation));
            }
        }
        Ok(reservations)
    }

    fn read_envelope(&self, path: &Path, extension: &str) -> Result<RequestEnvelope, LedgerError> {
        let bytes = read_bounded(path, self.entry_max_bytes)?;
        let end = bytes
            .iter()
            .rposition(|byte| *byte != 0)
            .map_or(0, |index| index + 1);
        let envelope: RequestEnvelope =
            serde_json::from_slice(&bytes[..end]).map_err(LedgerError::Json)?;
        if envelope.schema_version != SCHEMA_VERSION
            || path != self.path(envelope.request.id, extension)
        {
            return Err(LedgerError::InvalidCapacity);
        }
        Ok(envelope)
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

    fn quarantine(&self, path: &Path) -> Result<(), LedgerError> {
        let _state = self
            .state
            .lock()
            .map_err(|_| LedgerError::InvalidCapacity)?;
        let target = path.with_extension("conflict");
        let from = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| LedgerError::InvalidCapacity)?;
        let to = std::ffi::CString::new(target.as_os_str().as_encoded_bytes())
            .map_err(|_| LedgerError::InvalidCapacity)?;
        if unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(LedgerError::Io(std::io::Error::last_os_error()));
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

impl CheckpointGuard {
    fn begin(spool: Arc<Spool>, id: Uuid) -> Result<Self, LedgerError> {
        spool.begin_checkpoint(id)?;
        Ok(Self { spool, id })
    }
}

impl Drop for CheckpointGuard {
    fn drop(&mut self) {
        self.spool.end_checkpoint(self.id);
    }
}

fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, LedgerError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(LedgerError::Io)?;
    let metadata = file.metadata().map_err(LedgerError::Io)?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(LedgerError::InvalidCapacity);
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(LedgerError::Io)?;
    if u64::try_from(bytes.len()).map_or(true, |size| size > max_bytes) {
        return Err(LedgerError::RecordTooLarge);
    }
    Ok(bytes)
}

fn self_contained_envelope(path: &Path, entry_max_bytes: u64) -> Result<bool, LedgerError> {
    let bytes = read_bounded(path, entry_max_bytes)?;
    let end = bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |index| index + 1);
    if end == 0 {
        return Ok(false);
    }
    let Ok(envelope) = serde_json::from_slice::<RequestEnvelope>(&bytes[..end]) else {
        return Ok(false);
    };
    if envelope.schema_version != SCHEMA_VERSION {
        return Ok(false);
    }
    if path.file_stem().and_then(|value| value.to_str())
        != Some(envelope.request.id.simple().to_string().as_str())
    {
        return Err(LedgerError::InvalidCapacity);
    }
    Ok(true)
}

fn finalize_recovered_envelope(path: &Path, entry_max_bytes: u64) -> Result<(), LedgerError> {
    let bytes = read_bounded(path, entry_max_bytes)?;
    let end = bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |index| index + 1);
    let mut envelope: RequestEnvelope =
        serde_json::from_slice(&bytes[..end]).map_err(LedgerError::Json)?;
    if envelope.schema_version != SCHEMA_VERSION
        || path.file_stem().and_then(|value| value.to_str())
            != Some(envelope.request.id.simple().to_string().as_str())
    {
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

    #[test]
    fn attribution_accepts_only_one_short_printable_header() {
        use axum::http::{HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();
        headers.insert("x-apg-task-id", HeaderValue::from_static("task-123"));
        headers.insert(
            "x-apg-prompt-id",
            HeaderValue::from_str(&"x".repeat(129)).unwrap(),
        );
        headers.insert(
            "x-apg-agent-name",
            HeaderValue::from_bytes(&[0x80]).unwrap(),
        );
        headers.append("x-apg-client-version", HeaderValue::from_static("one"));
        headers.append("x-apg-client-version", HeaderValue::from_static("two"));
        let attribution = RequestAttribution::from_headers(&headers);
        assert_eq!(attribution.task_id.as_deref(), Some("task-123"));
        assert!(attribution.prompt_id.is_none());
        assert!(attribution.agent_name.is_none());
        assert!(attribution.client_version.is_none());
        assert!(attribution.skill_set_id.is_none());
    }

    #[sqlx::test]
    async fn attribution_survives_ledger_ingestion_and_old_envelopes(pool: Pool<Postgres>) {
        let mut envelope = RequestEnvelope::started(request_start("attribution"));
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-apg-task-id",
            axum::http::HeaderValue::from_static("task-123"),
        );
        envelope.request.attribution = Some(RequestAttribution::from_headers(&headers));
        let id = envelope.request.id;
        let tenant = envelope.request.tenant_id;
        let now = Utc::now();
        envelope.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: vec![],
            completed_at: now,
        });
        let payload = serde_json::json!([envelope]);
        for _ in 0..2 {
            sqlx::query("SELECT ingest_usage_envelopes($1)")
                .bind(&payload)
                .execute(&pool)
                .await
                .unwrap();
        }
        let task: String = sqlx::query_scalar(
            "SELECT task_id FROM request_attribution WHERE tenant_id=$1 AND gateway_request_id=$2",
        )
        .bind(tenant)
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(task, "task-123");
        assert!(sqlx::query("UPDATE request_attribution SET task_id='changed' WHERE tenant_id=$1 AND gateway_request_id=$2")
            .bind(tenant).bind(id).execute(&pool).await.is_err());
        let mut old = RequestEnvelope::started(request_start("legacy-attribution"));
        old.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: vec![],
            completed_at: Utc::now(),
        });
        let mut payload = serde_json::json!([old]);
        payload[0]["request"]
            .as_object_mut()
            .unwrap()
            .remove("attribution");
        sqlx::query("SELECT ingest_usage_envelopes($1)")
            .bind(payload)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[sqlx::test]
    async fn capture_backup_lock_defers_ledger_flush_until_release(pool: Pool<Postgres>) {
        let directory = test_directory("backup-ledger-race");
        let capture_directory = test_directory("backup-capture-lock");
        std::fs::create_dir_all(&capture_directory).unwrap();
        let ledger = Ledger::open_coordinated(
            pool.clone(),
            directory.clone(),
            192 * 1024,
            32 * 1024,
            Some(capture_directory.clone()),
        )
        .unwrap();
        let mut envelope = RequestEnvelope::started(request_start("backup-ledger-race"));
        let id = envelope.request.id;
        ledger.inner.spool.reserve(id, &envelope).unwrap();
        envelope.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: vec![],
            completed_at: Utc::now(),
        });
        ledger.inner.spool.finish(id, &envelope).unwrap();
        assert_eq!(ready_count(&directory), 1);
        let backup = crate::capture_backup_barrier::try_exclusive(&capture_directory).unwrap();
        assert!(matches!(
            ledger.flush_startup().await,
            Err(LedgerError::BackupBusy)
        ));
        ledger.schedule_flush(&tokio::runtime::Handle::current());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(ledger.inner.database_writes_enabled.load(Ordering::Acquire));
        assert_eq!(ready_count(&directory), 1);
        drop(backup);
        tokio::time::timeout(std::time::Duration::from_secs(5), ledger.wait_for_idle())
            .await
            .unwrap();
        assert_eq!(ready_count(&directory), 0);
        let failed_directory = test_directory("backup-ledger-unavailable");
        let unavailable = Ledger::open_coordinated(
            pool,
            failed_directory.clone(),
            192 * 1024,
            32 * 1024,
            Some(capture_directory.clone()),
        )
        .unwrap();
        std::fs::remove_file(capture_directory.join(".capture-backup.lock")).unwrap();
        std::fs::create_dir(capture_directory.join(".capture-backup.lock")).unwrap();
        let mut failed = RequestEnvelope::started(request_start("backup-ledger-unavailable"));
        let failed_id = failed.request.id;
        unavailable.inner.spool.reserve(failed_id, &failed).unwrap();
        failed.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: vec![],
            completed_at: Utc::now(),
        });
        unavailable.inner.spool.finish(failed_id, &failed).unwrap();
        unavailable.schedule_flush(&tokio::runtime::Handle::current());
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            unavailable.wait_for_idle(),
        )
        .await
        .unwrap();
        assert!(
            !unavailable
                .inner
                .database_writes_enabled
                .load(Ordering::Acquire)
        );
        assert_eq!(ready_count(&failed_directory), 1);
    }

    #[tokio::test]
    async fn startup_recovery_waits_for_backup_lock_before_touching_spool() {
        let directory = test_directory("backup-startup-lock");
        std::fs::create_dir_all(&directory).unwrap();
        let leftover = directory.join("fixture.tmp");
        std::fs::write(&leftover, b"unfinished").unwrap();
        let backup = crate::capture_backup_barrier::try_exclusive(&directory).unwrap();
        let pool = db::pool("postgres://gateway@127.0.0.1:1/gateway", 2).unwrap();
        let start = Ledger::open_coordinated(
            pool.clone(),
            directory.clone(),
            192 * 1024,
            32 * 1024,
            Some(directory.clone()),
        );
        assert!(matches!(start, Err(LedgerError::BackupBusy)));
        assert_eq!(std::fs::read(&leftover).unwrap(), b"unfinished");
        drop(backup);
        Ledger::open_coordinated(
            pool,
            directory.clone(),
            192 * 1024,
            32 * 1024,
            Some(directory),
        )
        .unwrap();
        assert!(!leftover.exists());
    }

    #[tokio::test]
    async fn idle_wait_includes_flush_pending_between_workers() {
        let directory = test_directory("flush-pending-idle");
        let pool = db::pool("postgres://gateway@127.0.0.1:1/gateway", 2).unwrap();
        let ledger = Ledger::open(pool, directory.clone(), 192 * 1024, 32 * 1024).unwrap();
        ledger.inner.flush_pending.store(true, Ordering::Release);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                ledger.wait_for_idle()
            )
            .await
            .is_err()
        );
        ledger.schedule_flush(&tokio::runtime::Handle::current());
        tokio::time::timeout(std::time::Duration::from_secs(3), ledger.wait_for_idle())
            .await
            .unwrap();
        assert!(!ledger.inner.flush_pending.load(Ordering::Acquire));
        ledger
            .inner
            .database_writes_enabled
            .store(false, Ordering::Release);
        ledger.inner.flush_pending.store(true, Ordering::Release);
        tokio::time::timeout(std::time::Duration::from_secs(1), ledger.wait_for_idle())
            .await
            .unwrap();
        drop(ledger);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[sqlx::test]
    async fn cancelled_finish_waits_for_queued_publish(pool: Pool<Postgres>) {
        let options = (*pool.connect_options()).clone();
        std::thread::spawn(move || {
            let directory = test_directory("cancelled-finish-publish");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(1)
                .build()
                .unwrap();
            runtime.block_on(async {
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(2)
                    .connect_lazy_with(options);
                let ledger =
                    Ledger::open(pool.clone(), directory.clone(), 192 * 1024, 32 * 1024).unwrap();
                let start = request_start("cancelled-finish-publish");
                let tenant = start.principal.tenant_id;
                let reservation = ledger.reserve(start).await.unwrap();
                let id = reservation.id.unwrap();
                let (started, started_rx) = tokio::sync::oneshot::channel();
                let (release, release_rx) = std::sync::mpsc::channel();
                let first = tokio::task::spawn_blocking(move || {
                    started.send(()).unwrap();
                    release_rx.recv().unwrap();
                });
                started_rx.await.unwrap();
                let mut finish = Box::pin(reservation.finish(RequestCompletion {
                    status: "completed".to_owned(),
                    selected_model: None,
                    selected_provider: None,
                    attempts: vec![],
                    completed_at: Utc::now(),
                }));
                std::future::poll_fn(|cx| {
                    assert!(std::future::Future::poll(finish.as_mut(), cx).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                release.send(()).unwrap();
                first.await.unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    while ledger
                        .inner
                        .spool
                        .checkpointing
                        .lock()
                        .unwrap()
                        .contains(&id)
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                let (started, started_rx) = tokio::sync::oneshot::channel();
                let (release, release_rx) = std::sync::mpsc::channel();
                let blocker = tokio::task::spawn_blocking(move || {
                    started.send(()).unwrap();
                    release_rx.recv().unwrap();
                });
                started_rx.await.unwrap();
                std::future::poll_fn(|cx| {
                    assert!(std::future::Future::poll(finish.as_mut(), cx).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                drop(finish);
                assert!(
                    tokio::time::timeout(
                        std::time::Duration::from_millis(100),
                        ledger.wait_for_finalizations()
                    )
                    .await
                    .is_err()
                );
                release.send(()).unwrap();
                blocker.await.unwrap();
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    ledger.wait_for_finalizations(),
                )
                .await
                .unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(5), ledger.wait_for_idle())
                    .await
                    .unwrap();
                let status = sqlx::query_file_scalar!(
                    "queries/test_cancelled_ledger_status.sql",
                    tenant,
                    "cancelled-finish-publish"
                )
                .fetch_one(&pool)
                .await
                .unwrap();
                assert_eq!(status, "completed");
                assert_eq!(ready_count(&directory), 0);
                pool.close().await;
                drop(ledger);
                std::fs::remove_dir_all(&directory).unwrap();
            });
        })
        .join()
        .unwrap();
    }

    #[test]
    fn cancelled_publish_failure_is_logged() {
        if std::env::var_os("GATEWAY_TEST_PUBLISH_LOG_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ledger::tests::cancelled_publish_failure_is_logged",
                    "--nocapture",
                ])
                .env("GATEWAY_TEST_PUBLISH_LOG_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
            return;
        }
        let buffer = crate::logging::capture::Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(buffer.clone())
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        let directory = test_directory("cancelled-publish-failure");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let pool = db::pool("postgres://gateway@127.0.0.1:1/gateway", 2).unwrap();
            let ledger = Ledger::open(pool, directory.clone(), 192 * 1024, 32 * 1024).unwrap();
            let reservation = ledger
                .reserve(request_start("publish-failure"))
                .await
                .unwrap();
            let id = reservation.id.unwrap();
            let (started, started_rx) = tokio::sync::oneshot::channel();
            let (release, release_rx) = std::sync::mpsc::channel();
            let first = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            started_rx.await.unwrap();
            let mut finish = Box::pin(reservation.finish(RequestCompletion {
                status: "completed".to_owned(),
                selected_model: None,
                selected_provider: None,
                attempts: vec![],
                completed_at: Utc::now(),
            }));
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(finish.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            release.send(()).unwrap();
            first.await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while ledger
                    .inner
                    .spool
                    .checkpointing
                    .lock()
                    .unwrap()
                    .contains(&id)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let (started, started_rx) = tokio::sync::oneshot::channel();
            let (release, release_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            started_rx.await.unwrap();
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(finish.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            drop(finish);
            std::fs::rename(
                ledger.inner.spool.path(id, "working"),
                ledger.inner.spool.path(id, "held"),
            )
            .unwrap();
            release.send(()).unwrap();
            blocker.await.unwrap();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                ledger.wait_for_finalizations(),
            )
            .await
            .unwrap();
            let lines = buffer.lines();
            let errors = lines
                .iter()
                .filter(|line| line.to_string().contains("ledger_publish_failed"))
                .collect::<Vec<_>>();
            assert_eq!(errors.len(), 1);
            assert!(
                !errors[0]
                    .to_string()
                    .contains(&directory.to_string_lossy().to_string())
            );
            drop(ledger);
            std::fs::remove_dir_all(&directory).unwrap();
        });
    }

    #[test]
    fn cancelled_queued_checkpoint_releases_its_registration() {
        let directory = test_directory("cancelled-queued-checkpoint");
        let spool = Arc::new(Spool::open(directory.clone(), 192 * 1024, 32 * 1024).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let blocker = runtime.spawn_blocking(move || {
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        ready_rx.recv().unwrap();
        let id = Uuid::new_v4();
        let checkpoint = CheckpointGuard::begin(spool.clone(), id).unwrap();
        let queued = runtime.spawn_blocking(move || drop(checkpoint));
        queued.abort();
        release_tx.send(()).unwrap();
        runtime.block_on(async {
            blocker.await.unwrap();
            assert!(matches!(queued.await, Err(error) if error.is_cancelled()));
        });
        assert!(!spool.checkpointing.lock().unwrap().contains(&id));
        drop(spool);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn overlapping_checkpoints_wait_for_every_writer() {
        let directory = test_directory("overlapping-checkpoints");
        let spool = Arc::new(Spool::open(directory.clone(), 192 * 1024, 32 * 1024).unwrap());
        let id = Uuid::new_v4();
        let first = CheckpointGuard::begin(spool.clone(), id).unwrap();
        assert!(matches!(
            CheckpointGuard::begin(spool.clone(), id),
            Err(LedgerError::CheckpointBusy)
        ));
        let waiting_spool = spool.clone();
        let mut waiter =
            tokio::spawn(async move { waiting_spool.wait_for_checkpoint_async(id).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut waiter)
                .await
                .is_err()
        );
        drop(first);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let next = CheckpointGuard::begin(spool.clone(), id).unwrap();
        drop(next);
        drop(spool);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[sqlx::test]
    async fn later_checkpoint_waits_for_prior_cancelled_writer(pool: Pool<Postgres>) {
        let directory = test_directory("later-checkpoint");
        let ledger = Ledger::open_coordinated(
            pool,
            directory.clone(),
            192 * 1024,
            32 * 1024,
            Some(directory.clone()),
        )
        .unwrap();
        let mut reservation = ledger
            .reserve(request_start("later-checkpoint"))
            .await
            .unwrap();
        let active =
            CheckpointGuard::begin(ledger.inner.spool.clone(), reservation.id.unwrap()).unwrap();
        let mut later = tokio::spawn(async move {
            reservation
                .checkpoint_queue_entered(Utc::now())
                .await
                .unwrap();
            reservation
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut later)
                .await
                .is_err()
        );
        drop(active);
        let reservation = tokio::time::timeout(std::time::Duration::from_secs(5), later)
            .await
            .unwrap()
            .unwrap();
        reservation
            .finish(RequestCompletion {
                status: "completed".to_owned(),
                selected_model: None,
                selected_provider: None,
                attempts: vec![],
                completed_at: Utc::now(),
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), ledger.wait_for_idle())
            .await
            .unwrap();
        assert_eq!(ready_count(&directory), 0);
    }

    #[sqlx::test]
    async fn backup_excludes_active_and_cancelled_ledger_requests(pool: Pool<Postgres>) {
        let directory = test_directory("backup-ledger-active");
        let ledger = Ledger::open_coordinated(
            pool,
            directory.clone(),
            192 * 1024,
            32 * 1024,
            Some(directory.clone()),
        )
        .unwrap();
        let backup = crate::capture_backup_barrier::try_exclusive(&directory).unwrap();
        assert!(matches!(
            ledger.reserve(request_start("backup-blocked")).await,
            Err(LedgerError::BackupBusy)
        ));
        drop(backup);
        let reservation = ledger
            .reserve(request_start("backup-active"))
            .await
            .unwrap();
        assert!(crate::capture_backup_barrier::try_exclusive(&directory).is_err());
        drop(reservation);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ledger.wait_for_finalizations(),
        )
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), ledger.wait_for_idle())
            .await
            .unwrap();
        let backup = crate::capture_backup_barrier::try_exclusive(&directory).unwrap();
        drop(backup);
        let finished = ledger
            .reserve(request_start("backup-finished"))
            .await
            .unwrap();
        assert!(crate::capture_backup_barrier::try_exclusive(&directory).is_err());
        finished
            .finish(RequestCompletion {
                status: "completed".to_owned(),
                selected_model: None,
                selected_provider: None,
                attempts: vec![],
                completed_at: Utc::now(),
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), ledger.wait_for_idle())
            .await
            .unwrap();
        assert!(crate::capture_backup_barrier::try_exclusive(&directory).is_ok());
    }

    #[sqlx::test]
    async fn cancelled_reserve_is_finalized_after_background_write(pool: Pool<Postgres>) {
        let directory = test_directory("cancelled-reserve");
        let ledger = Ledger::open_coordinated(
            pool.clone(),
            directory.clone(),
            192 * 1024,
            32 * 1024,
            Some(directory.clone()),
        )
        .unwrap();
        let spool = ledger.inner.spool.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let holder = std::thread::spawn(move || {
            let _lock = spool.state.lock().unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        ready_rx.recv().unwrap();
        let start = request_start("cancelled-reserve-fixture");
        let tenant = start.principal.tenant_id;
        let worker = ledger.clone();
        let request = tokio::spawn(async move { worker.reserve(start).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while Arc::strong_count(&ledger.inner.spool) < 3 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(crate::capture_backup_barrier::try_exclusive(&directory).is_err());
        request.abort();
        assert!(matches!(request.await, Err(error) if error.is_cancelled()));
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                ledger.wait_for_finalizations()
            )
            .await
            .is_err()
        );
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(status) = sqlx::query_file_scalar!(
                    "queries/test_cancelled_ledger_status.sql",
                    tenant,
                    "cancelled-reserve-fixture"
                )
                .fetch_optional(&pool)
                .await
                .unwrap()
                {
                    break status;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(status, "cancelled");
        ledger.wait_for_idle().await;
        assert_eq!(ready_count(&directory), 0);
        assert!(crate::capture_backup_barrier::try_exclusive(&directory).is_ok());
    }

    #[sqlx::test]
    async fn concurrent_cancelled_checkpoints_finish_with_two_blocking_threads(
        pool: Pool<Postgres>,
    ) {
        let options = (*pool.connect_options()).clone();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .max_blocking_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runtime.block_on(async move {
                    let pool = sqlx::postgres::PgPoolOptions::new()
                        .max_connections(4)
                        .connect_with(options)
                        .await
                        .unwrap();
                    let directory = test_directory("concurrent-cancelled-checkpoints");
                    let ledger = Ledger::open_coordinated(
                        pool.clone(),
                        directory.clone(),
                        4 * 1024 * 1024,
                        32 * 1024,
                        Some(directory.clone()),
                    )
                    .unwrap();
                    let mut reservations = Vec::new();
                    for index in 0..16 {
                        let request_id = format!("concurrent-cancel-{index}");
                        let start = request_start(&request_id);
                        let tenant = start.principal.tenant_id;
                        reservations.push((
                            tenant,
                            request_id,
                            ledger.reserve(start).await.unwrap(),
                        ));
                    }
                    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(2);
                    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
                    let releases = Arc::new(std::sync::Mutex::new(release_rx));
                    let mut blockers = Vec::new();
                    for _ in 0..2 {
                        let ready = ready_tx.clone();
                        let release = releases.clone();
                        blockers.push(tokio::task::spawn_blocking(move || {
                            ready.send(()).unwrap();
                            release.lock().unwrap().recv().unwrap();
                        }));
                    }
                    ready_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    ready_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    let mut ids = Vec::new();
                    let mut requests = Vec::new();
                    for (tenant, request_id, mut reservation) in reservations {
                        ids.push((tenant, request_id));
                        requests.push(tokio::spawn(async move {
                            reservation.checkpoint_queue_entered(Utc::now()).await
                        }));
                    }
                    tokio::time::timeout(std::time::Duration::from_secs(5), async {
                        loop {
                            if ledger.inner.spool.checkpointing.lock().unwrap().len() == 16 {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .unwrap();
                    for request in &requests {
                        request.abort();
                    }
                    for request in requests {
                        assert!(matches!(request.await, Err(error) if error.is_cancelled()));
                    }
                    assert!(
                        tokio::time::timeout(
                            std::time::Duration::from_millis(100),
                            ledger.wait_for_finalizations()
                        )
                        .await
                        .is_err()
                    );
                    release_tx.send(()).unwrap();
                    release_tx.send(()).unwrap();
                    for blocker in blockers {
                        blocker.await.unwrap();
                    }
                    tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        ledger.wait_for_finalizations(),
                    )
                    .await
                    .unwrap();
                    tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        ledger.wait_for_idle(),
                    )
                    .await
                    .unwrap();
                    for (tenant, request_id) in ids {
                        let status = sqlx::query_file_scalar!(
                            "queries/test_cancelled_ledger_status.sql",
                            tenant,
                            request_id
                        )
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                        assert_eq!(status, "cancelled");
                    }
                    assert!(crate::capture_backup_barrier::try_exclusive(&directory).is_ok());
                    pool.close().await;
                    drop(ledger);
                    std::fs::remove_dir_all(directory).unwrap();
                })
            }));
            runtime.shutdown_timeout(std::time::Duration::from_secs(1));
            outcome.unwrap();
        });
        worker.join().unwrap();
    }

    #[sqlx::test]
    async fn pending_checkpoints_do_not_block_all_finalization_threads(pool: Pool<Postgres>) {
        let options = (*pool.connect_options()).clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .max_blocking_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runtime.block_on(async move {
                    let pool = sqlx::postgres::PgPoolOptions::new()
                        .max_connections(4)
                        .connect_with(options)
                        .await
                        .unwrap();
                    let directory = test_directory("checkpoint-pool-starvation");
                    let ledger = Ledger::open_coordinated(
                        pool.clone(),
                        directory.clone(),
                        1024 * 1024,
                        32 * 1024,
                        Some(directory.clone()),
                    )
                    .unwrap();
                    let first = ledger
                        .reserve(request_start("checkpoint-wait-1"))
                        .await
                        .unwrap();
                    let second = ledger
                        .reserve(request_start("checkpoint-wait-2"))
                        .await
                        .unwrap();
                    let first_checkpoint =
                        CheckpointGuard::begin(ledger.inner.spool.clone(), first.id.unwrap())
                            .unwrap();
                    let second_checkpoint =
                        CheckpointGuard::begin(ledger.inner.spool.clone(), second.id.unwrap())
                            .unwrap();
                    drop(first);
                    drop(second);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let probe = tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        tokio::task::spawn_blocking(|| ()),
                    )
                    .await;
                    drop(first_checkpoint);
                    drop(second_checkpoint);
                    tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        ledger.wait_for_finalizations(),
                    )
                    .await
                    .unwrap();
                    tokio::time::timeout(std::time::Duration::from_secs(5), ledger.wait_for_idle())
                        .await
                        .unwrap();
                    pool.close().await;
                    drop(ledger);
                    std::fs::remove_dir_all(directory).unwrap();
                    assert!(probe.is_ok());
                })
            }));
            runtime.shutdown_timeout(std::time::Duration::from_secs(1));
            outcome.unwrap();
        })
        .join()
        .unwrap();
    }

    #[sqlx::test]
    async fn rejected_requests_keep_attribution(pool: Pool<Postgres>) {
        let directory = test_directory("rejected-attribution");
        let ledger = Ledger::open(pool.clone(), directory, 192 * 1024, 32 * 1024).unwrap();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-apg-task-id",
            axum::http::HeaderValue::from_static("rejected-task"),
        );
        let attribution = RequestAttribution::from_headers(&headers);
        let initial = ledger
            .reserve_with_quota(
                request_start("initial-attribution"),
                None,
                None,
                Some(&attribution),
            )
            .await
            .unwrap();
        let bytes = fs::read(ledger.inner.spool.path(initial.id.unwrap(), "reserved")).unwrap();
        let end = bytes
            .iter()
            .rposition(|byte| *byte != 0)
            .map_or(0, |index| index + 1);
        let stored: RequestEnvelope = serde_json::from_slice(&bytes[..end]).unwrap();
        assert_eq!(
            stored.request.attribution.unwrap().task_id.as_deref(),
            Some("rejected-task")
        );
        drop(initial);
        ledger
            .reject(
                request_start("rejected-attribution"),
                "messages",
                None,
                None,
                &attribution,
            )
            .await
            .unwrap();
        ledger.wait_for_idle().await;
        let task: String = sqlx::query_scalar("SELECT a.task_id FROM request_attribution a JOIN gateway_requests r ON r.tenant_id=a.tenant_id AND r.id=a.gateway_request_id WHERE r.request_id='rejected-attribution' AND r.status='rejected'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(task, "rejected-task");
    }
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
    fn task_routing_is_nested_under_the_routing_decision() {
        let snapshot = RequestPolicySnapshot::unevaluated("test")
            .with_routing("jev")
            .with_task_routing(serde_json::json!({
                "original_model": "client-model",
                "final_model": "reviewed-model",
                "classification": {"task": "debugging", "confidence": 0.9}
            }));

        assert_eq!(
            snapshot.decision["routing"]["task_classification"]["original_model"],
            "client-model"
        );
        assert_eq!(
            snapshot.decision["routing"]["task_classification"]["classification"]["task"],
            "debugging"
        );
    }

    #[test]
    fn gateway_overhead_excludes_queue_and_provider_time() {
        let received_at = Utc::now();
        let mut start = request_start("gateway-overhead");
        start.created_at = received_at;
        let mut envelope = RequestEnvelope::started(start);
        envelope.request.queue_duration_ms = Some(40);
        envelope.request.client_first_byte_at =
            Some(received_at + chrono::Duration::milliseconds(230));

        let provider_request_at = received_at + chrono::Duration::milliseconds(100);
        let mut attempt = ProviderAttempt::begin(
            "test-provider".to_owned(),
            "company-default".to_owned(),
            None,
            provider_request_at,
            TokenUsage::default(),
        );
        attempt.observe_first_byte(received_at + chrono::Duration::milliseconds(200));
        attempt.conclude(
            "completed",
            None,
            received_at + chrono::Duration::milliseconds(500),
        );

        envelope.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: Some("company-default".to_owned()),
            selected_provider: Some("test-provider".to_owned()),
            attempts: vec![attempt],
            completed_at: received_at + chrono::Duration::milliseconds(500),
        });

        assert_eq!(envelope.request.gateway_overhead_ms, Some(90));
        assert_eq!(envelope.request.total_latency_ms, Some(500));
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
        envelope.request.policy_decision = Some(
            RequestPolicySnapshot::unevaluated("test")
                .with_routing("jev")
                .with_task_routing(serde_json::json!({
                    "original_model": "client-model",
                    "final_model": "reviewed-model"
                }))
                .decision,
        );
        let request_id = envelope.request.id;
        spool.reserve(request_id, &envelope).unwrap();
        envelope.request.requested_parameters = Some(serde_json::json!({
            "schema_version": 1,
            "operation": "anthropic_messages",
            "model": "company-default",
            "message_count": 1,
            "content_part_count": 1,
            "text_chars": 15,
            "image_count": 0,
            "tool_count": 0,
            "has_system": false,
            "has_structured_output": false
        }));
        envelope.request.provider_parameters = Some(serde_json::json!({
            "schema_version": 1,
            "operation": "anthropic_messages",
            "model": "provider-model",
            "message_count": 1,
            "content_part_count": 1,
            "text_chars": 15,
            "image_count": 0,
            "tool_count": 0,
            "has_system": false,
            "has_structured_output": false
        }));
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
        envelope.finalized = Some(true);
        spool.finish(request_id, &envelope).unwrap();
        let ready = fs::read_to_string(spool.path(request_id, "ready")).unwrap();
        assert!(!ready.contains("sentinel-prompt"));
        drop(spool);

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
        second.finalized = Some(true);
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

        drop(unavailable);
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
                (SELECT count(*) FROM request_parameter_snapshots) AS parameter_snapshots,
                (SELECT cached_input_tokens FROM usage_records) AS cached,
                (SELECT cache_write_input_tokens FROM usage_records) AS cache_write,
                (SELECT reasoning_tokens FROM usage_records) AS reasoning,
                (SELECT policy_decision->'routing'->'task_classification'->>'original_model'
                 FROM gateway_requests WHERE request_id = 'recovery-request') AS original_model
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<i64, _>("requests"), 2);
        assert_eq!(row.get::<i64, _>("attempts"), 1);
        assert_eq!(row.get::<i64, _>("usage"), 1);
        assert_eq!(row.get::<i64, _>("parameter_snapshots"), 2);
        assert_eq!(row.get::<i64, _>("cached"), 4);
        assert_eq!(row.get::<i64, _>("cache_write"), 2);
        assert_eq!(row.get::<i64, _>("reasoning"), 1);
        assert_eq!(row.get::<String, _>("original_model"), "client-model");

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
    async fn cancelling_blocked_startup_flush_preserves_usage_for_replay(pool: Pool<Postgres>) {
        let directory = test_directory("cancelled-startup-flush");
        let ledger = Ledger::open(pool.clone(), directory.clone(), 192 * 1024, 32 * 1024).unwrap();
        let mut envelope = RequestEnvelope::started(request_start("cancelled-startup-flush"));
        let request_id = envelope.request.id;
        let tenant = envelope.request.tenant_id;
        let now = Utc::now();
        ledger.inner.spool.reserve(request_id, &envelope).unwrap();
        envelope.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: vec![ProviderAttempt::completed(
                CompletedAttempt {
                    provider: "test-provider".to_owned(),
                    model: "test-model".to_owned(),
                    provider_request_id: None,
                    provider_response_id: None,
                    started_at: now,
                    completed_at: now,
                    status: "completed".to_owned(),
                    error_type: None,
                },
                br#"{"usage":{"prompt_tokens":10,"completion_tokens":3}}"#,
            )],
            completed_at: now,
        });
        ledger.inner.spool.finish(request_id, &envelope).unwrap();
        let mut lock = pool.begin().await.unwrap();
        let locked = sqlx::query_file!("queries/test_lock_usage_tenant.sql", tenant)
            .fetch_one(&mut *lock)
            .await
            .unwrap();
        let flushing = ledger.clone();
        let task = tokio::spawn(async move { flushing.flush_startup().await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if sqlx::query_file_scalar!("queries/test_waiting_usage_ingest.sql", locked.pid)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(ready_count(&directory), 1);
        let before = sqlx::query_file!("queries/test_usage_replay_counts.sql", request_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((before.requests, before.attempts, before.usage), (0, 0, 0));
        lock.rollback().await.unwrap();
        assert_eq!(ledger.flush_startup().await.unwrap(), 1);
        assert_eq!(ready_count(&directory), 0);
        let after = sqlx::query_file!("queries/test_usage_replay_counts.sql", request_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((after.requests, after.attempts, after.usage), (1, 1, 1));
        ledger.inner.spool.reserve(request_id, &envelope).unwrap();
        ledger.inner.spool.finish(request_id, &envelope).unwrap();
        assert_eq!(ledger.flush_startup().await.unwrap(), 1);
        let replayed = sqlx::query_file!("queries/test_usage_replay_counts.sql", request_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            (replayed.requests, replayed.attempts, replayed.usage),
            (1, 1, 1)
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn conflict_record_counts_toward_capacity_after_restart() {
        let directory = test_directory("conflict-capacity");
        fs::create_dir_all(&directory).unwrap();
        let conflict = directory.join(format!("{}.conflict", Uuid::new_v4()));
        fs::write(&conflict, b"retained conflict").unwrap();
        let spool = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let envelope = RequestEnvelope::started(request_start("capacity-check"));
        assert!(matches!(
            spool.reserve(envelope.request.id, &envelope),
            Err(LedgerError::SpoolFull)
        ));
        drop(spool);
        let reopened = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        assert!(matches!(
            reopened.reserve(envelope.request.id, &envelope),
            Err(LedgerError::SpoolFull)
        ));
        assert_eq!(fs::read(&conflict).unwrap(), b"retained conflict");
        drop(reopened);
        let ready = conflict.with_extension("ready");
        fs::write(&ready, b"other record").unwrap();
        assert!(matches!(
            Spool::open(
                directory.clone(),
                MIN_ENTRY_MAX_BYTES * 3,
                MIN_ENTRY_MAX_BYTES
            ),
            Err(LedgerError::InvalidCapacity)
        ));
        assert_eq!(fs::read(&conflict).unwrap(), b"retained conflict");
        assert_eq!(fs::read(&ready).unwrap(), b"other record");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quarantining_never_overwrites_an_existing_conflict() {
        let directory = test_directory("conflict-no-overwrite");
        let spool = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 6,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let id = Uuid::new_v4();
        let ready = spool.path(id, "ready");
        let conflict = spool.path(id, "conflict");
        fs::write(&ready, b"new record").unwrap();
        fs::write(&conflict, b"old record").unwrap();
        assert!(spool.quarantine(&ready).is_err());
        assert_eq!(fs::read(&ready).unwrap(), b"new record");
        assert_eq!(fs::read(&conflict).unwrap(), b"old record");
        drop(spool);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn conflict_scan_rejects_links_and_oversized_files_without_removing_them() {
        let directory = test_directory("unsafe-conflict");
        let spool = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 9,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let target = directory.join("target");
        fs::write(&target, b"preserved").unwrap();
        let link = spool.path(Uuid::new_v4(), "conflict");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(
            spool.conflict_reservations(),
            Err(LedgerError::Io(_))
        ));
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&target).unwrap(), b"preserved");
        fs::remove_file(&link).unwrap();
        let oversized = spool.path(Uuid::new_v4(), "conflict");
        fs::write(&oversized, vec![1; MIN_ENTRY_MAX_BYTES as usize + 1]).unwrap();
        assert!(matches!(
            spool.conflict_reservations(),
            Err(LedgerError::InvalidCapacity)
        ));
        assert_eq!(
            fs::metadata(&oversized).unwrap().len(),
            MIN_ENTRY_MAX_BYTES + 1
        );
        drop(spool);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_rejects_ready_symlink_and_preserves_target() {
        let directory = test_directory("recovery-link");
        fs::create_dir_all(&directory).unwrap();
        let envelope = RequestEnvelope::started(request_start("recovery-link"));
        let data = serde_json::to_vec(&envelope).unwrap();
        let target = directory.join("original");
        fs::write(&target, &data).unwrap();
        let link = directory.join(format!("{}.ready", envelope.request.id.simple()));
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            Spool::open(
                directory.clone(),
                MIN_ENTRY_MAX_BYTES * 3,
                MIN_ENTRY_MAX_BYTES
            )
            .is_err()
        );
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&target).unwrap(), data);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_dropped_spool_unlocks_with_an_inherited_descriptor() {
        let directory = test_directory("spool-inherited-lock");
        let spool = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let inherited = spool._instance_lock.try_clone().unwrap();
        assert!(matches!(
            Spool::open(
                directory.clone(),
                MIN_ENTRY_MAX_BYTES * 3,
                MIN_ENTRY_MAX_BYTES
            ),
            Err(LedgerError::SpoolBusy)
        ));
        drop(spool);
        let reopened = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        drop(reopened);
        drop(inherited);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn second_spool_cannot_recover_an_active_request() {
        let directory = test_directory("single-writer");
        let first = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let envelope = RequestEnvelope::started(request_start("single-writer"));
        first.reserve(envelope.request.id, &envelope).unwrap();
        assert_eq!(
            std::process::Command::new("flock")
                .args([
                    "-n",
                    directory.join(".ledger-spool.lock").to_str().unwrap(),
                    "true"
                ])
                .status()
                .unwrap()
                .code(),
            Some(1)
        );
        let reserved = first.path(envelope.request.id, "reserved");
        let original = fs::read(&reserved).unwrap();
        assert!(matches!(
            Spool::open(
                directory.clone(),
                MIN_ENTRY_MAX_BYTES * 3,
                MIN_ENTRY_MAX_BYTES
            ),
            Err(LedgerError::SpoolBusy)
        ));
        assert_eq!(fs::read(&reserved).unwrap(), original);
        assert!(!first.path(envelope.request.id, "ready").exists());
        drop(first);
        assert!(
            std::process::Command::new("flock")
                .args([
                    "-n",
                    directory.join(".ledger-spool.lock").to_str().unwrap(),
                    "true"
                ])
                .status()
                .unwrap()
                .success()
        );
        let reopened = Spool::open(
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        assert_eq!(reopened.ready().unwrap().len(), 1);
        drop(reopened);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn spool_rejects_unsafe_lock_files_without_modifying_them() {
        for kind in ["symlink", "permissions", "hardlink"] {
            let directory = test_directory("unsafe-instance-lock");
            fs::create_dir_all(&directory).unwrap();
            let original = directory.join("original");
            fs::write(&original, b"preserved").unwrap();
            fs::set_permissions(&original, std::fs::Permissions::from_mode(0o600)).unwrap();
            let lock = directory.join(".ledger-spool.lock");
            match kind {
                "symlink" => std::os::unix::fs::symlink(&original, &lock).unwrap(),
                "hardlink" => fs::hard_link(&original, &lock).unwrap(),
                _ => {
                    fs::write(&lock, b"preserved").unwrap();
                    fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
                }
            }
            assert!(
                Spool::open(
                    directory.clone(),
                    MIN_ENTRY_MAX_BYTES * 3,
                    MIN_ENTRY_MAX_BYTES
                )
                .is_err()
            );
            assert_eq!(fs::read(&original).unwrap(), b"preserved");
            assert_eq!(fs::read(&lock).unwrap(), b"preserved");
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn startup_rejects_unsafe_pending_files_without_changing_them() {
        for extension in ["reserved", "working", "ready"] {
            let directory = test_directory("recovery-unsafe");
            fs::create_dir_all(&directory).unwrap();
            let id = Uuid::new_v4();
            let path = directory.join(format!("{}.{}", id.simple(), extension));
            let target = directory.join("original");
            fs::write(&target, b"preserved").unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(
                Spool::open(
                    directory.clone(),
                    MIN_ENTRY_MAX_BYTES * 3,
                    MIN_ENTRY_MAX_BYTES
                )
                .is_err()
            );
            assert!(
                fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(fs::read(&target).unwrap(), b"preserved");
            fs::remove_dir_all(directory).unwrap();
        }
        let directory = test_directory("recovery-oversize");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{}.ready", Uuid::new_v4().simple()));
        fs::write(&path, vec![1; MIN_ENTRY_MAX_BYTES as usize + 1]).unwrap();
        assert!(
            Spool::open(
                directory.clone(),
                MIN_ENTRY_MAX_BYTES * 3,
                MIN_ENTRY_MAX_BYTES
            )
            .is_err()
        );
        assert_eq!(fs::metadata(&path).unwrap().len(), MIN_ENTRY_MAX_BYTES + 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_rejects_named_pipe_and_mismatched_request_id() {
        let directory = test_directory("recovery-fifo");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{}.ready", Uuid::new_v4().simple()));
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Spool::open(
                directory.clone(),
                MIN_ENTRY_MAX_BYTES * 3,
                MIN_ENTRY_MAX_BYTES
            )
            .is_err()
        );
        assert!(std::os::unix::fs::FileTypeExt::is_fifo(
            &fs::symlink_metadata(&path).unwrap().file_type()
        ));
        fs::remove_dir_all(directory).unwrap();
        let directory = test_directory("recovery-id");
        fs::create_dir_all(&directory).unwrap();
        let envelope = RequestEnvelope::started(request_start("recovery-id"));
        let path = directory.join(format!("{}.ready", Uuid::new_v4().simple()));
        let data = serde_json::to_vec(&envelope).unwrap();
        fs::write(&path, &data).unwrap();
        assert!(
            Spool::open(
                directory.clone(),
                MIN_ENTRY_MAX_BYTES * 3,
                MIN_ENTRY_MAX_BYTES
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), data);
        fs::remove_dir_all(directory).unwrap();
    }

    #[sqlx::test]
    async fn replay_rejects_changed_content_and_rolls_back_mixed_batch(pool: Pool<Postgres>) {
        let mut original = RequestEnvelope::started(request_start("content-original"));
        let now = original.request.created_at + chrono::Duration::seconds(1);
        original.request.requested_parameters = Some(serde_json::json!({"model":"requested"}));
        original.request.provider_parameters = Some(serde_json::json!({"model":"provider"}));
        original.request.attribution = Some(RequestAttribution {
            task_id: Some("original-task".to_owned()),
            ..RequestAttribution::default()
        });
        original.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: vec![ProviderAttempt::completed(
                CompletedAttempt {
                    provider: "test-provider".to_owned(),
                    model: "test-model".to_owned(),
                    provider_request_id: None,
                    provider_response_id: None,
                    started_at: now,
                    completed_at: now,
                    status: "completed".to_owned(),
                    error_type: None,
                },
                br#"{"usage":{"prompt_tokens":2,"completion_tokens":1}}"#,
            )],
            completed_at: now,
        });
        let original_id = original.request.id;
        let original = serde_json::to_value(original).unwrap();
        for _ in 0..2 {
            sqlx::query_file!(
                "queries/test_ingest_usage_replay.sql",
                serde_json::json!([original])
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        let mut changes = Vec::new();
        for (pointer, value) in [
            ("/usage_records/0/input_tokens", serde_json::json!(50)),
            (
                "/attempts/0/provider_request_id",
                serde_json::json!("changed"),
            ),
            ("/request/status", serde_json::json!("failed")),
            ("/request/attribution/task_id", serde_json::json!("changed")),
            (
                "/request/requested_parameters/model",
                serde_json::json!("changed"),
            ),
            (
                "/request/provider_parameters/model",
                serde_json::json!("changed"),
            ),
            ("/request/attribution", Value::Null),
            ("/request/requested_parameters", Value::Null),
            ("/attempts", serde_json::json!([])),
            ("/usage_records", serde_json::json!([])),
            ("/request/id", serde_json::json!(Uuid::new_v4())),
            (
                "/usage_records/0/provider_attempt_id",
                serde_json::json!(Uuid::new_v4()),
            ),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            changes.push((pointer, changed));
        }
        for (case, changed) in changes {
            let healthy = RequestEnvelope::started(request_start("mixed-healthy"));
            let healthy_id = healthy.request.id;
            let error = sqlx::query_file!(
                "queries/test_ingest_usage_replay.sql",
                serde_json::json!([healthy, changed])
            )
            .execute(&pool)
            .await
            .expect_err(case);
            assert_eq!(
                error.as_database_error().unwrap().code().as_deref(),
                Some("GW409"),
                "{case}"
            );
            let counts = sqlx::query_file!("queries/test_usage_replay_counts.sql", healthy_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(
                (counts.requests, counts.attempts, counts.usage),
                (0, 0, 0),
                "{case}"
            );
            sqlx::query_file!(
                "queries/test_ingest_usage_replay.sql",
                serde_json::json!([original])
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        let counts = sqlx::query_file!("queries/test_usage_replay_counts.sql", original_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((counts.requests, counts.attempts, counts.usage), (1, 1, 1));
        let request_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let mut left = original.clone();
        left["request"]["id"] = serde_json::json!(request_id);
        left["request"]["request_id"] = serde_json::json!("global-id-collision");
        left["attempts"][0]["id"] = serde_json::json!(attempt_id);
        left["attempts"][0]["gateway_request_id"] = serde_json::json!(request_id);
        left["usage_records"][0]["id"] = serde_json::json!(Uuid::new_v4());
        left["usage_records"][0]["gateway_request_id"] = serde_json::json!(request_id);
        left["usage_records"][0]["provider_attempt_id"] = serde_json::json!(attempt_id);
        let mut right = left.clone();
        let tenant = serde_json::json!(Uuid::new_v4());
        right["request"]["tenant_id"] = tenant.clone();
        right["attempts"][0]["tenant_id"] = tenant.clone();
        right["usage_records"][0]["tenant_id"] = tenant;
        let left = serde_json::json!([left]);
        let right = serde_json::json!([right]);
        let left_query = sqlx::query_file!("queries/test_ingest_usage_replay.sql", left);
        let right_query = sqlx::query_file!("queries/test_ingest_usage_replay.sql", right);
        let (left, right) = tokio::join!(left_query.execute(&pool), right_query.execute(&pool));
        assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
        let error = left.err().or_else(|| right.err()).unwrap();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("GW409")
        );
        let counts = sqlx::query_file!("queries/test_usage_replay_counts.sql", request_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((counts.requests, counts.attempts, counts.usage), (1, 1, 1));
    }

    #[sqlx::test]
    async fn conflicting_usage_is_preserved_and_other_records_are_ingested(pool: Pool<Postgres>) {
        let directory = test_directory("conflict-batch");
        let ledger = Ledger::open(pool.clone(), directory.clone(), 192 * 1024, 32 * 1024).unwrap();
        let now = Utc::now() + chrono::Duration::seconds(60);
        let mut original = RequestEnvelope::started(request_start("conflict-original"));
        let original_id = original.request.id;
        original.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: Vec::new(),
            completed_at: now,
        });
        ledger.inner.spool.reserve(original_id, &original).unwrap();
        ledger.inner.spool.finish(original_id, &original).unwrap();
        assert_eq!(ledger.flush_startup().await.unwrap(), 1);
        let mut conflict = original.clone();
        conflict.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: vec![ProviderAttempt::completed(
                CompletedAttempt {
                    provider: "test-provider".to_owned(),
                    model: "test-model".to_owned(),
                    provider_request_id: None,
                    provider_response_id: None,
                    started_at: now,
                    completed_at: now,
                    status: "completed".to_owned(),
                    error_type: None,
                },
                br#"{"usage":{"prompt_tokens":10,"completion_tokens":3}}"#,
            )],
            completed_at: now,
        });
        ledger.inner.spool.reserve(original_id, &conflict).unwrap();
        ledger.inner.spool.finish(original_id, &conflict).unwrap();
        let mut healthy = RequestEnvelope::started(request_start("conflict-healthy"));
        let healthy_id = healthy.request.id;
        healthy.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: Vec::new(),
            completed_at: now,
        });
        ledger.inner.spool.reserve(healthy_id, &healthy).unwrap();
        ledger.inner.spool.finish(healthy_id, &healthy).unwrap();
        let conflict_path = ledger.inner.spool.path(original_id, "ready");
        let healthy_path = ledger.inner.spool.path(healthy_id, "ready");
        let conflict_bytes = fs::read(&conflict_path).unwrap();
        assert_eq!(ledger.flush_startup().await.unwrap(), 1);
        let quarantined = conflict_path.with_extension("conflict");
        assert_eq!(fs::read(&quarantined).unwrap(), conflict_bytes);
        assert!(!conflict_path.exists());
        assert!(!healthy_path.exists());
        assert_eq!(ready_count(&directory), 0);
        let original_counts =
            sqlx::query_file!("queries/test_usage_replay_counts.sql", original_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (
                original_counts.requests,
                original_counts.attempts,
                original_counts.usage
            ),
            (1, 0, 0)
        );
        let counts = sqlx::query_file!("queries/test_usage_replay_counts.sql", healthy_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((counts.requests, counts.attempts, counts.usage), (1, 0, 0));
        drop(ledger);
        let reopened = Ledger::open(pool, directory.clone(), 192 * 1024, 32 * 1024).unwrap();
        assert_eq!(reopened.flush_startup().await.unwrap(), 0);
        assert_eq!(fs::read(quarantined).unwrap(), conflict_bytes);
        assert_eq!(reopened.inner.spool.state.lock().unwrap().slots_used, 1);
        drop(reopened);
        fs::remove_dir_all(directory).unwrap();
    }

    #[sqlx::test]
    async fn ambiguous_reservation_records_are_preserved_together(pool: Pool<Postgres>) {
        let directory = test_directory("ambiguous-reservation");
        let ledger = Ledger::open(
            pool.clone(),
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 9,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let reservation = Uuid::new_v4();
        let now = Utc::now() + chrono::Duration::seconds(60);
        let mut ids = Vec::new();
        for name in ["ambiguous-first", "ambiguous-second"] {
            let mut envelope = RequestEnvelope::started(request_start(name));
            envelope.request.quota_reservation_id = Some(reservation);
            let id = envelope.request.id;
            envelope.complete(RequestCompletion {
                status: "completed".to_owned(),
                selected_model: None,
                selected_provider: None,
                attempts: Vec::new(),
                completed_at: now,
            });
            ledger.inner.spool.reserve(id, &envelope).unwrap();
            ledger.inner.spool.finish(id, &envelope).unwrap();
            ids.push(id);
        }
        let mut healthy = RequestEnvelope::started(request_start("ambiguous-healthy"));
        let healthy_id = healthy.request.id;
        healthy.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: Vec::new(),
            completed_at: now,
        });
        ledger.inner.spool.reserve(healthy_id, &healthy).unwrap();
        ledger.inner.spool.finish(healthy_id, &healthy).unwrap();
        assert_eq!(ledger.flush_startup().await.unwrap(), 1);
        for id in ids {
            assert!(ledger.inner.spool.path(id, "conflict").exists());
            let counts = sqlx::query_file!("queries/test_usage_replay_counts.sql", id)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!((counts.requests, counts.attempts, counts.usage), (0, 0, 0));
        }
        let counts = sqlx::query_file!("queries/test_usage_replay_counts.sql", healthy_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(counts.requests, 1);
        drop(ledger);
        fs::remove_dir_all(directory).unwrap();
    }

    #[sqlx::test]
    async fn partially_quarantined_reservation_stays_grouped_after_restart(pool: Pool<Postgres>) {
        let directory = test_directory("partial-conflict-group");
        let ledger = Ledger::open(
            pool.clone(),
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 9,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let reservation = Uuid::new_v4();
        let now = Utc::now() + chrono::Duration::seconds(60);
        let mut ids = Vec::new();
        for name in ["partial-first", "partial-second"] {
            let mut envelope = RequestEnvelope::started(request_start(name));
            envelope.request.quota_reservation_id = Some(reservation);
            let id = envelope.request.id;
            envelope.complete(RequestCompletion {
                status: "completed".to_owned(),
                selected_model: None,
                selected_provider: None,
                attempts: Vec::new(),
                completed_at: now,
            });
            ledger.inner.spool.reserve(id, &envelope).unwrap();
            ledger.inner.spool.finish(id, &envelope).unwrap();
            ids.push(id);
        }
        let tenant = request_start("partial-tenant").principal.tenant_id;
        sqlx::query_file!(
            "queries/test_insert_partial_conflict_reservation.sql",
            reservation,
            tenant,
            ids[1]
        )
        .execute(&pool)
        .await
        .unwrap();
        let first = ledger.inner.spool.path(ids[0], "ready");
        let first_bytes = fs::read(&first).unwrap();
        ledger.inner.spool.quarantine(&first).unwrap();
        drop(ledger);
        let reopened = Ledger::open(
            pool.clone(),
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 9,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let mut healthy = RequestEnvelope::started(request_start("partial-healthy"));
        let healthy_id = healthy.request.id;
        healthy.complete(RequestCompletion {
            status: "completed".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: Vec::new(),
            completed_at: now,
        });
        reopened.inner.spool.reserve(healthy_id, &healthy).unwrap();
        reopened.inner.spool.finish(healthy_id, &healthy).unwrap();
        assert_eq!(reopened.flush_startup().await.unwrap(), 1);
        assert_eq!(
            fs::read(reopened.inner.spool.path(ids[0], "conflict")).unwrap(),
            first_bytes
        );
        for id in ids {
            assert!(reopened.inner.spool.path(id, "conflict").exists());
            let counts = sqlx::query_file!("queries/test_usage_replay_counts.sql", id)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(counts.requests, 0);
        }
        let healthy = sqlx::query_file!("queries/test_usage_replay_counts.sql", healthy_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(healthy.requests, 1);
        drop(reopened);
        fs::remove_dir_all(directory).unwrap();
    }

    #[sqlx::test]
    async fn startup_flush_drains_multiple_bounded_batches(pool: Pool<Postgres>) {
        let directory = test_directory("bounded-startup");
        let ledger = Ledger::open(
            pool.clone(),
            directory.clone(),
            MIN_ENTRY_MAX_BYTES * 3 * 35,
            MIN_ENTRY_MAX_BYTES,
        )
        .unwrap();
        let now = Utc::now() + chrono::Duration::seconds(60);
        for index in 0..35 {
            let mut envelope = RequestEnvelope::started(request_start(&format!("bounded-{index}")));
            let id = envelope.request.id;
            envelope.complete(RequestCompletion {
                status: "completed".to_owned(),
                selected_model: None,
                selected_provider: None,
                attempts: Vec::new(),
                completed_at: now,
            });
            ledger.inner.spool.reserve(id, &envelope).unwrap();
            ledger.inner.spool.finish(id, &envelope).unwrap();
        }
        assert_eq!(ledger.flush_startup().await.unwrap(), 35);
        assert_eq!(ready_count(&directory), 0);
        drop(ledger);
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
