use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::{
    policy::WorkloadClass,
    quota::{QuotaConcurrencyConstraint, QuotaMode},
};

const WEIGHTS: [u32; 6] = [20, 10, 6, 3, 2, 1];

#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    pub active_interactive_reserved: usize,
    pub active_shared: usize,
    pub active_background_reserved: usize,
    pub queue_interactive_reserved: usize,
    pub queue_shared: usize,
    pub queue_background_reserved: usize,
    pub queue_per_tenant: usize,
    pub queue_per_credential: usize,
    pub per_provider: usize,
    pub per_model: usize,
    pub per_user: usize,
    pub per_device: usize,
    pub per_client: usize,
    pub per_agent: usize,
    pub per_credential: usize,
    pub per_workload: usize,
    pub hard_queue_timeout: Duration,
    pub aging_interval: Duration,
    pub aging_enabled: bool,
    pub reservations_enabled: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            active_interactive_reserved: 12,
            active_shared: 4,
            active_background_reserved: 4,
            queue_interactive_reserved: 64,
            queue_shared: 128,
            queue_background_reserved: 64,
            queue_per_tenant: 64,
            queue_per_credential: 32,
            per_provider: 20,
            per_model: 20,
            per_user: 8,
            per_device: 8,
            per_client: 8,
            per_agent: 8,
            per_credential: 8,
            per_workload: 20,
            hard_queue_timeout: Duration::from_secs(30),
            aging_interval: Duration::from_secs(1),
            aging_enabled: true,
            reservations_enabled: true,
        }
    }
}

impl SchedulerConfig {
    pub fn validate(&self) -> Result<(), SchedulerError> {
        let positive = [
            self.active_interactive_reserved,
            self.active_shared,
            self.active_background_reserved,
            self.queue_interactive_reserved,
            self.queue_shared,
            self.queue_background_reserved,
            self.queue_per_tenant,
            self.queue_per_credential,
            self.per_provider,
            self.per_model,
            self.per_user,
            self.per_device,
            self.per_client,
            self.per_agent,
            self.per_credential,
            self.per_workload,
        ];
        if positive.contains(&0)
            || self.hard_queue_timeout.is_zero()
            || self.aging_interval.is_zero()
        {
            return Err(SchedulerError::InvalidConfiguration);
        }
        Ok(())
    }

    fn active_total(&self) -> usize {
        self.active_interactive_reserved + self.active_shared + self.active_background_reserved
    }

    fn queue_total(&self) -> usize {
        self.queue_interactive_reserved + self.queue_shared + self.queue_background_reserved
    }
}

#[derive(Clone, Debug)]
pub struct CacheAffinity {
    pub expires_at: DateTime<Utc>,
    pub source: &'static str,
}

#[derive(Clone, Debug)]
pub struct SchedulerRequest {
    pub tenant_id: Uuid,
    pub provider_id: Uuid,
    pub model_id: Uuid,
    pub user_id: Option<Uuid>,
    pub device_id: Option<Uuid>,
    pub client_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    pub credential_id: Uuid,
    pub workload: WorkloadClass,
    pub quality: String,
    pub received_at: DateTime<Utc>,
    pub policy_deadline: Option<DateTime<Utc>>,
    pub cache_affinity: Option<CacheAffinity>,
    pub quota: Vec<QuotaConcurrencyConstraint>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Family {
    Interactive,
    Background,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PoolKind {
    Interactive,
    Shared,
    Background,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Dimension {
    Global,
    Provider(Uuid),
    Model(Uuid),
    User(Uuid),
    Device(Uuid),
    Client(Uuid),
    Agent(Uuid),
    Credential(Uuid),
    Workload(WorkloadClass),
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

struct Inner {
    config: SchedulerConfig,
    started_at: DateTime<Utc>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    closed: bool,
    sequence: u64,
    waiters: VecDeque<Waiter>,
    queued_pools: PoolCounts,
    queued_tenants: HashMap<(Uuid, Family), usize>,
    queued_credentials: HashMap<(Uuid, Family), usize>,
    active_pools: PoolCounts,
    active: HashMap<Dimension, usize>,
    quota_active: HashMap<Uuid, usize>,
    fairness_credit: HashMap<WorkloadClass, i64>,
    active_tenants: HashMap<Uuid, usize>,
    active_workloads: HashMap<(Uuid, WorkloadClass), usize>,
    active_qualities: HashMap<(Uuid, String), usize>,
    active_providers: HashMap<(Uuid, Uuid), usize>,
    tenant_stats: HashMap<Uuid, TenantStats>,
}

#[derive(Default)]
struct PoolCounts {
    interactive: usize,
    shared: usize,
    background: usize,
}

#[derive(Default)]
struct TenantStats {
    accepted: u64,
    queued: u64,
    rejected: u64,
    expired: u64,
}

struct Waiter {
    id: u64,
    request: SchedulerRequest,
    queue_pool: PoolKind,
    entered_at: DateTime<Utc>,
    deadline: DateTime<Utc>,
    sender: oneshot::Sender<Result<SchedulerLease, SchedulerError>>,
}

pub struct QueueTicket {
    inner: Arc<Inner>,
    id: u64,
    deadline: DateTime<Utc>,
    receiver: oneshot::Receiver<Result<SchedulerLease, SchedulerError>>,
    active: bool,
}

pub struct SchedulerLease {
    inner: Arc<Inner>,
    allocation: Option<Allocation>,
}

struct Allocation {
    request: SchedulerRequest,
    active_pool: PoolKind,
    dimensions: Vec<Dimension>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SchedulerError {
    #[error("scheduler queue is full")]
    QueueFull,
    #[error("scheduler queue deadline expired")]
    Expired,
    #[error("request exceeds an active hard concurrency quota")]
    HardQuota,
    #[error("scheduler is shutting down")]
    Closed,
    #[error("scheduler configuration is invalid")]
    InvalidConfiguration,
    #[error("scheduler state is unavailable")]
    Unavailable,
}

impl Scheduler {
    pub fn new(config: SchedulerConfig) -> Result<Self, SchedulerError> {
        config.validate()?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                started_at: Utc::now(),
                state: Mutex::new(State::default()),
            }),
        })
    }

    pub fn try_acquire(
        &self,
        request: &SchedulerRequest,
    ) -> Result<Option<SchedulerLease>, SchedulerError> {
        let now = Utc::now();
        let deadline = self.effective_deadline(request)?;
        if deadline <= now {
            self.record(request.tenant_id, |stats| stats.expired += 1)?;
            return Err(SchedulerError::Expired);
        }
        let mut state = self.lock()?;
        if state.closed {
            return Err(SchedulerError::Closed);
        }
        if hard_quota_blocked(&state, request) {
            return Err(SchedulerError::HardQuota);
        }
        if !state.waiters.is_empty() {
            return Ok(None);
        }
        let Some(pool) = active_pool(&self.inner.config, &state, request) else {
            return Ok(None);
        };
        let dimensions = dimensions(request);
        if !fits(&self.inner.config, &state, request, &dimensions) {
            return Ok(None);
        }
        allocate(&mut state, request, pool, &dimensions);
        state
            .tenant_stats
            .entry(request.tenant_id)
            .or_default()
            .accepted += 1;
        Ok(Some(SchedulerLease {
            inner: Arc::clone(&self.inner),
            allocation: Some(Allocation {
                request: request.clone(),
                active_pool: pool,
                dimensions,
            }),
        }))
    }

    pub fn enqueue(&self, request: SchedulerRequest) -> Result<QueueTicket, SchedulerError> {
        let now = Utc::now();
        let deadline = self.effective_deadline(&request)?;
        if deadline <= now {
            self.record(request.tenant_id, |stats| stats.expired += 1)?;
            return Err(SchedulerError::Expired);
        }
        let mut state = self.lock()?;
        if state.closed {
            return Err(SchedulerError::Closed);
        }
        if hard_quota_blocked(&state, &request) {
            state
                .tenant_stats
                .entry(request.tenant_id)
                .or_default()
                .rejected += 1;
            return Err(SchedulerError::HardQuota);
        }
        let queue_pool = queue_pool(&self.inner.config, &state, &request)
            .or_else(|| make_fair_share_room(&self.inner.config, &mut state, &request))
            .ok_or_else(|| {
                state
                    .tenant_stats
                    .entry(request.tenant_id)
                    .or_default()
                    .rejected += 1;
                SchedulerError::QueueFull
            })?;
        state.sequence = state.sequence.wrapping_add(1);
        let id = state.sequence;
        let (sender, receiver) = oneshot::channel();
        increment_pool(&mut state.queued_pools, queue_pool);
        let request_family = family(request.workload);
        *state
            .queued_tenants
            .entry((request.tenant_id, request_family))
            .or_default() += 1;
        *state
            .queued_credentials
            .entry((request.credential_id, request_family))
            .or_default() += 1;
        state
            .tenant_stats
            .entry(request.tenant_id)
            .or_default()
            .queued += 1;
        state.waiters.push_back(Waiter {
            id,
            request,
            queue_pool,
            entered_at: now,
            deadline,
            sender,
        });
        dispatch(&self.inner, &mut state, now);
        Ok(QueueTicket {
            inner: Arc::clone(&self.inner),
            id,
            deadline,
            receiver,
            active: true,
        })
    }

    pub fn close(&self) -> Result<(), SchedulerError> {
        let mut state = self.lock()?;
        state.closed = true;
        while let Some(waiter) = state.waiters.pop_front() {
            release_queue(&mut state, &waiter);
            let _ = waiter.sender.send(Err(SchedulerError::Closed));
        }
        Ok(())
    }

    pub fn snapshot(&self, tenant_id: Uuid) -> Result<Value, SchedulerError> {
        let state = self.lock()?;
        let mut workloads = HashMap::<&'static str, usize>::new();
        let mut cache_expiring = 0usize;
        let mut cache_sources = HashMap::<&'static str, usize>::new();
        for waiter in state
            .waiters
            .iter()
            .filter(|value| value.request.tenant_id == tenant_id)
        {
            *workloads
                .entry(workload_name(waiter.request.workload))
                .or_default() += 1;
            if waiter
                .request
                .cache_affinity
                .as_ref()
                .is_some_and(|cache| cache.expires_at <= waiter.deadline)
            {
                cache_expiring += 1;
            }
            if let Some(cache) = &waiter.request.cache_affinity {
                *cache_sources.entry(cache.source).or_default() += 1;
            }
        }
        let active_tenant = state.active_tenants.get(&tenant_id).copied().unwrap_or(0);
        let active_by_workload = state
            .active_workloads
            .iter()
            .filter(|((tenant, _), _)| *tenant == tenant_id)
            .map(|((_, workload), count)| (workload_name(*workload), *count))
            .collect::<HashMap<_, _>>();
        let active_by_quality = state
            .active_qualities
            .iter()
            .filter(|((tenant, _), _)| *tenant == tenant_id)
            .map(|((_, quality), count)| (quality.clone(), *count))
            .collect::<HashMap<_, _>>();
        let active_by_provider = state
            .active_providers
            .iter()
            .filter(|((tenant, _), _)| *tenant == tenant_id)
            .map(|((_, provider), count)| (provider.to_string(), *count))
            .collect::<HashMap<_, _>>();
        let stats = state.tenant_stats.get(&tenant_id);
        Ok(json!({
            "available": true,
            "scheduler_started_at": self.inner.started_at,
            "closed": state.closed,
            "active_tenant": active_tenant,
            "active_capacity": self.inner.config.active_total(),
            "active_by_workload": active_by_workload,
            "active_by_quality": active_by_quality,
            "active_by_provider": active_by_provider,
            "queued": state.waiters.iter().filter(|value| value.request.tenant_id == tenant_id).count(),
            "queued_by_workload": workloads,
            "cache_expiring_before_dispatch_deadline": cache_expiring,
            "queued_cache_affinity_sources": cache_sources,
            "accepted_since_start": stats.map_or(0, |value| value.accepted),
            "queued_since_start": stats.map_or(0, |value| value.queued),
            "rejected_since_start": stats.map_or(0, |value| value.rejected),
            "expired_since_start": stats.map_or(0, |value| value.expired),
        }))
    }

    pub fn effective_deadline(
        &self,
        request: &SchedulerRequest,
    ) -> Result<DateTime<Utc>, SchedulerError> {
        let hard = chrono::Duration::from_std(self.inner.config.hard_queue_timeout)
            .map_err(|_| SchedulerError::InvalidConfiguration)?;
        let hard = request.received_at + hard;
        Ok(request
            .policy_deadline
            .map_or(hard, |value| value.min(hard)))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, SchedulerError> {
        self.inner
            .state
            .lock()
            .map_err(|_| SchedulerError::Unavailable)
    }

    fn record(
        &self,
        tenant_id: Uuid,
        update: impl FnOnce(&mut TenantStats),
    ) -> Result<(), SchedulerError> {
        let mut state = self.lock()?;
        update(state.tenant_stats.entry(tenant_id).or_default());
        Ok(())
    }
}

impl QueueTicket {
    pub async fn wait(mut self) -> Result<SchedulerLease, SchedulerError> {
        let deadline = self.deadline;
        let sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(
            std::time::Instant::now() + (deadline - Utc::now()).to_std().unwrap_or_default(),
        ));
        tokio::pin!(sleep);
        let result = tokio::select! {
            result = &mut self.receiver => result.unwrap_or(Err(SchedulerError::Closed)),
            () = &mut sleep => {
                remove_waiter(&self.inner, self.id, true)?;
                Err(SchedulerError::Expired)
            }
        };
        self.active = false;
        result
    }
}

impl Drop for QueueTicket {
    fn drop(&mut self) {
        if self.active {
            let _ = remove_waiter(&self.inner, self.id, false);
        }
    }
}

impl Drop for SchedulerLease {
    fn drop(&mut self) {
        let Some(allocation) = self.allocation.take() else {
            return;
        };
        let Ok(mut state) = self.inner.state.lock() else {
            tracing::error!(error.type = "scheduler_release_failed", "scheduler state is poisoned");
            return;
        };
        release_active(&mut state, &allocation);
        dispatch(&self.inner, &mut state, Utc::now());
    }
}

fn remove_waiter(inner: &Arc<Inner>, id: u64, expired: bool) -> Result<(), SchedulerError> {
    let mut state = inner
        .state
        .lock()
        .map_err(|_| SchedulerError::Unavailable)?;
    if let Some(index) = state.waiters.iter().position(|value| value.id == id)
        && let Some(waiter) = state.waiters.remove(index)
    {
        release_queue(&mut state, &waiter);
        if expired {
            state
                .tenant_stats
                .entry(waiter.request.tenant_id)
                .or_default()
                .expired += 1;
        }
        dispatch(inner, &mut state, Utc::now());
    }
    Ok(())
}

fn dispatch(inner: &Arc<Inner>, state: &mut State, now: DateTime<Utc>) {
    if state.closed {
        return;
    }
    loop {
        let mut candidates = state
            .waiters
            .iter()
            .enumerate()
            .filter(|(_, waiter)| waiter.deadline > now)
            .filter_map(|(index, waiter)| {
                let dimensions = dimensions(&waiter.request);
                let pool = active_pool(&inner.config, state, &waiter.request)?;
                fits(&inner.config, state, &waiter.request, &dimensions).then_some((
                    index,
                    waiter.request.workload,
                    effective_weight(&inner.config, waiter, now),
                    waiter
                        .request
                        .cache_affinity
                        .as_ref()
                        .map_or(DateTime::<Utc>::MAX_UTC, |value| value.expires_at),
                    waiter.id,
                    pool,
                    dimensions,
                ))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| right.2.cmp(&left.2))
                .then_with(|| left.3.cmp(&right.3))
                .then_with(|| left.4.cmp(&right.4))
        });
        candidates.dedup_by_key(|candidate| candidate.1);
        let classes = candidates
            .iter()
            .map(|candidate| (candidate.1, candidate.2, candidate.4))
            .collect::<Vec<_>>();
        let Some(selected) = choose_fair_workload(&mut state.fairness_credit, &classes) else {
            break;
        };
        let Some(position) = candidates
            .iter()
            .position(|candidate| candidate.1 == selected)
        else {
            break;
        };
        let (index, _workload, _, _, _, active_pool, dimensions) = candidates.swap_remove(position);
        let Some(waiter) = state.waiters.remove(index) else {
            break;
        };
        release_queue(state, &waiter);
        allocate(state, &waiter.request, active_pool, &dimensions);
        state
            .tenant_stats
            .entry(waiter.request.tenant_id)
            .or_default()
            .accepted += 1;
        let lease = SchedulerLease {
            inner: Arc::clone(inner),
            allocation: Some(Allocation {
                request: waiter.request.clone(),
                active_pool,
                dimensions,
            }),
        };
        if let Err(result) = waiter.sender.send(Ok(lease)) {
            drop(result);
        }
        let remaining = state
            .waiters
            .iter()
            .map(|waiter| waiter.request.workload)
            .collect::<HashSet<_>>();
        state
            .fairness_credit
            .retain(|workload, _| remaining.contains(workload));
    }
}

fn choose_fair_workload(
    credits: &mut HashMap<WorkloadClass, i64>,
    classes: &[(WorkloadClass, u32, u64)],
) -> Option<WorkloadClass> {
    let total_weight = classes
        .iter()
        .map(|(_, weight, _)| i64::from(*weight))
        .sum::<i64>();
    for (workload, weight, _) in classes {
        *credits.entry(*workload).or_default() += i64::from(*weight);
    }
    let selected = classes
        .iter()
        .max_by(|left, right| {
            credits
                .get(&left.0)
                .copied()
                .unwrap_or_default()
                .cmp(&credits.get(&right.0).copied().unwrap_or_default())
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| right.2.cmp(&left.2))
        })?
        .0;
    *credits.entry(selected).or_default() -= total_weight;
    Some(selected)
}

fn family(workload: WorkloadClass) -> Family {
    match workload {
        WorkloadClass::Critical | WorkloadClass::Interactive | WorkloadClass::Foreground => {
            Family::Interactive
        }
        WorkloadClass::Automation | WorkloadClass::Background | WorkloadClass::Batch => {
            Family::Background
        }
    }
}

fn workload_name(workload: WorkloadClass) -> &'static str {
    match workload {
        WorkloadClass::Critical => "critical",
        WorkloadClass::Interactive => "interactive",
        WorkloadClass::Foreground => "foreground",
        WorkloadClass::Automation => "automation",
        WorkloadClass::Background => "background",
        WorkloadClass::Batch => "batch",
    }
}

fn weight(workload: WorkloadClass) -> u32 {
    match workload {
        WorkloadClass::Critical => WEIGHTS[0],
        WorkloadClass::Interactive => WEIGHTS[1],
        WorkloadClass::Foreground => WEIGHTS[2],
        WorkloadClass::Automation => WEIGHTS[3],
        WorkloadClass::Background => WEIGHTS[4],
        WorkloadClass::Batch => WEIGHTS[5],
    }
}

fn effective_weight(config: &SchedulerConfig, waiter: &Waiter, now: DateTime<Utc>) -> u32 {
    let base = weight(waiter.request.workload);
    if !config.aging_enabled {
        return base;
    }
    let interval_ms = config.aging_interval.as_millis().max(1);
    let waited_ms = (now - waiter.entered_at).num_milliseconds().max(0) as u128;
    base.saturating_add((waited_ms / interval_ms) as u32)
        .min(WEIGHTS[0])
}

fn queue_pool(
    config: &SchedulerConfig,
    state: &State,
    request: &SchedulerRequest,
) -> Option<PoolKind> {
    let request_family = family(request.workload);
    if state.waiters.len() >= config.queue_total()
        || state
            .queued_tenants
            .get(&(request.tenant_id, request_family))
            .copied()
            .unwrap_or(0)
            >= config.queue_per_tenant
        || state
            .queued_credentials
            .get(&(request.credential_id, request_family))
            .copied()
            .unwrap_or(0)
            >= config.queue_per_credential
    {
        return None;
    }
    if !config.reservations_enabled {
        return (state.queued_pools.shared < config.queue_total()).then_some(PoolKind::Shared);
    }
    let own = match family(request.workload) {
        Family::Interactive => (
            PoolKind::Interactive,
            state.queued_pools.interactive,
            config.queue_interactive_reserved,
        ),
        Family::Background => (
            PoolKind::Background,
            state.queued_pools.background,
            config.queue_background_reserved,
        ),
    };
    if own.1 < own.2 {
        Some(own.0)
    } else if state.queued_pools.shared < config.queue_shared {
        Some(PoolKind::Shared)
    } else {
        None
    }
}

fn make_fair_share_room(
    config: &SchedulerConfig,
    state: &mut State,
    request: &SchedulerRequest,
) -> Option<PoolKind> {
    let request_family = family(request.workload);
    let tenant_key = (request.tenant_id, request_family);
    let credential_key = (request.credential_id, request_family);
    if state.queued_tenants.get(&tenant_key).copied().unwrap_or(0) >= config.queue_per_tenant
        || state
            .queued_credentials
            .get(&credential_key)
            .copied()
            .unwrap_or(0)
            >= config.queue_per_credential
    {
        return None;
    }
    let tenant_is_new = !state.queued_tenants.contains_key(&tenant_key);
    let credential_is_new = !state.queued_credentials.contains_key(&credential_key);
    if !tenant_is_new && !credential_is_new {
        return None;
    }
    let victim_index = state
        .waiters
        .iter()
        .enumerate()
        .filter(|(_, waiter)| waiter.request.workload == request.workload)
        .filter(|(_, waiter)| {
            let waiter_tenant = (waiter.request.tenant_id, request_family);
            let waiter_credential = (waiter.request.credential_id, request_family);
            (tenant_is_new
                && state
                    .queued_tenants
                    .get(&waiter_tenant)
                    .copied()
                    .unwrap_or(0)
                    > 1)
                || (credential_is_new
                    && state
                        .queued_credentials
                        .get(&waiter_credential)
                        .copied()
                        .unwrap_or(0)
                        > 1)
        })
        .max_by_key(|(_, waiter)| waiter.id)
        .map(|(index, _)| index);
    let victim_index = victim_index?;
    let victim = state.waiters.remove(victim_index)?;
    release_queue(state, &victim);
    let queue_pool = queue_pool(config, state, request);
    if queue_pool.is_none() {
        increment_queue(state, &victim);
        state.waiters.insert(victim_index, victim);
        return None;
    }
    state
        .tenant_stats
        .entry(victim.request.tenant_id)
        .or_default()
        .rejected += 1;
    let _ = victim.sender.send(Err(SchedulerError::QueueFull));
    queue_pool
}

fn active_pool(
    config: &SchedulerConfig,
    state: &State,
    request: &SchedulerRequest,
) -> Option<PoolKind> {
    if state.active.get(&Dimension::Global).copied().unwrap_or(0) >= config.active_total() {
        return None;
    }
    if !config.reservations_enabled {
        return (state.active_pools.shared < config.active_total()).then_some(PoolKind::Shared);
    }
    let own = match family(request.workload) {
        Family::Interactive => (
            PoolKind::Interactive,
            state.active_pools.interactive,
            config.active_interactive_reserved,
        ),
        Family::Background => (
            PoolKind::Background,
            state.active_pools.background,
            config.active_background_reserved,
        ),
    };
    if own.1 < own.2 {
        Some(own.0)
    } else if state.active_pools.shared < config.active_shared {
        Some(PoolKind::Shared)
    } else {
        None
    }
}

fn dimensions(request: &SchedulerRequest) -> Vec<Dimension> {
    let mut values = vec![
        Dimension::Global,
        Dimension::Provider(request.provider_id),
        Dimension::Model(request.model_id),
        Dimension::Credential(request.credential_id),
        Dimension::Workload(request.workload),
    ];
    values.extend(request.user_id.map(Dimension::User));
    values.extend(request.device_id.map(Dimension::Device));
    values.extend(request.client_id.map(Dimension::Client));
    values.extend(request.agent_id.map(Dimension::Agent));
    values
}

fn fits(
    config: &SchedulerConfig,
    state: &State,
    request: &SchedulerRequest,
    dimensions: &[Dimension],
) -> bool {
    dimensions.iter().all(|dimension| {
        let limit = match dimension {
            Dimension::Global => config.active_total(),
            Dimension::Provider(_) => config.per_provider,
            Dimension::Model(_) => config.per_model,
            Dimension::User(_) => config.per_user,
            Dimension::Device(_) => config.per_device,
            Dimension::Client(_) => config.per_client,
            Dimension::Agent(_) => config.per_agent,
            Dimension::Credential(_) => config.per_credential,
            Dimension::Workload(_) => config.per_workload,
        };
        state.active.get(dimension).copied().unwrap_or(0) < limit
    }) && request.quota.iter().all(|constraint| {
        state
            .quota_active
            .get(&constraint.quota_id)
            .copied()
            .unwrap_or(0)
            < constraint.limit as usize
    })
}

fn hard_quota_blocked(state: &State, request: &SchedulerRequest) -> bool {
    request.quota.iter().any(|constraint| {
        constraint.mode == QuotaMode::Hard
            && state
                .quota_active
                .get(&constraint.quota_id)
                .copied()
                .unwrap_or(0)
                >= constraint.limit as usize
    })
}

fn allocate(
    state: &mut State,
    request: &SchedulerRequest,
    pool: PoolKind,
    dimensions: &[Dimension],
) {
    increment_pool(&mut state.active_pools, pool);
    for dimension in dimensions {
        *state.active.entry(dimension.clone()).or_default() += 1;
    }
    for constraint in &request.quota {
        *state.quota_active.entry(constraint.quota_id).or_default() += 1;
    }
    *state.active_tenants.entry(request.tenant_id).or_default() += 1;
    *state
        .active_workloads
        .entry((request.tenant_id, request.workload))
        .or_default() += 1;
    *state
        .active_qualities
        .entry((request.tenant_id, request.quality.clone()))
        .or_default() += 1;
    *state
        .active_providers
        .entry((request.tenant_id, request.provider_id))
        .or_default() += 1;
}

fn release_active(state: &mut State, allocation: &Allocation) {
    decrement_pool(&mut state.active_pools, allocation.active_pool);
    for dimension in &allocation.dimensions {
        decrement_map(&mut state.active, dimension);
    }
    for constraint in &allocation.request.quota {
        decrement_map(&mut state.quota_active, &constraint.quota_id);
    }
    decrement_map(&mut state.active_tenants, &allocation.request.tenant_id);
    decrement_map(
        &mut state.active_workloads,
        &(allocation.request.tenant_id, allocation.request.workload),
    );
    decrement_map(
        &mut state.active_qualities,
        &(
            allocation.request.tenant_id,
            allocation.request.quality.clone(),
        ),
    );
    decrement_map(
        &mut state.active_providers,
        &(allocation.request.tenant_id, allocation.request.provider_id),
    );
}

fn release_queue(state: &mut State, waiter: &Waiter) {
    decrement_pool(&mut state.queued_pools, waiter.queue_pool);
    let request_family = family(waiter.request.workload);
    decrement_map(
        &mut state.queued_tenants,
        &(waiter.request.tenant_id, request_family),
    );
    decrement_map(
        &mut state.queued_credentials,
        &(waiter.request.credential_id, request_family),
    );
}

fn increment_queue(state: &mut State, waiter: &Waiter) {
    increment_pool(&mut state.queued_pools, waiter.queue_pool);
    let request_family = family(waiter.request.workload);
    *state
        .queued_tenants
        .entry((waiter.request.tenant_id, request_family))
        .or_default() += 1;
    *state
        .queued_credentials
        .entry((waiter.request.credential_id, request_family))
        .or_default() += 1;
}

fn increment_pool(counts: &mut PoolCounts, pool: PoolKind) {
    match pool {
        PoolKind::Interactive => counts.interactive += 1,
        PoolKind::Shared => counts.shared += 1,
        PoolKind::Background => counts.background += 1,
    }
}

fn decrement_pool(counts: &mut PoolCounts, pool: PoolKind) {
    let value = match pool {
        PoolKind::Interactive => &mut counts.interactive,
        PoolKind::Shared => &mut counts.shared,
        PoolKind::Background => &mut counts.background,
    };
    *value = value.saturating_sub(1);
}

fn decrement_map<K: std::hash::Hash + Eq>(values: &mut HashMap<K, usize>, key: &K) {
    if let Some(value) = values.get_mut(key) {
        *value = value.saturating_sub(1);
        if *value == 0 {
            values.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(workload: WorkloadClass, tenant: Uuid, credential: Uuid) -> SchedulerRequest {
        SchedulerRequest {
            tenant_id: tenant,
            provider_id: Uuid::new_v4(),
            model_id: Uuid::new_v4(),
            user_id: Some(Uuid::new_v4()),
            device_id: Some(Uuid::new_v4()),
            client_id: Some(Uuid::new_v4()),
            agent_id: Some(Uuid::new_v4()),
            credential_id: credential,
            workload,
            quality: "standard".to_owned(),
            received_at: Utc::now(),
            policy_deadline: None,
            cache_affinity: None,
            quota: Vec::new(),
        }
    }

    #[tokio::test]
    async fn background_cannot_consume_interactive_reserved_execution_or_queue_cells() {
        let config = SchedulerConfig {
            active_interactive_reserved: 1,
            active_shared: 1,
            active_background_reserved: 1,
            queue_interactive_reserved: 1,
            queue_shared: 1,
            queue_background_reserved: 1,
            queue_per_tenant: 10,
            queue_per_credential: 10,
            ..SchedulerConfig::default()
        };
        let scheduler = Scheduler::new(config).unwrap();
        let tenant = Uuid::new_v4();
        let credential = Uuid::new_v4();
        let background = request(WorkloadClass::Background, tenant, credential);
        let _first = scheduler.try_acquire(&background).unwrap().unwrap();
        let _second = scheduler.try_acquire(&background).unwrap().unwrap();
        let _queued_reserved = scheduler.enqueue(background.clone()).unwrap();
        let _queued_shared = scheduler.enqueue(background.clone()).unwrap();
        assert!(matches!(
            scheduler.enqueue(background),
            Err(SchedulerError::QueueFull)
        ));
        let interactive = request(WorkloadClass::Interactive, tenant, credential);
        assert!(scheduler.enqueue(interactive).unwrap().wait().await.is_ok());
    }

    #[test]
    fn aging_reaches_critical_weight_but_never_changes_reserved_family() {
        let config = SchedulerConfig::default();
        let now = Utc::now();
        let (sender, _receiver) = oneshot::channel();
        let waiter = Waiter {
            id: 1,
            request: request(WorkloadClass::Batch, Uuid::new_v4(), Uuid::new_v4()),
            queue_pool: PoolKind::Background,
            entered_at: now - chrono::Duration::seconds(30),
            deadline: now + chrono::Duration::seconds(1),
            sender,
        };
        assert_eq!(effective_weight(&config, &waiter, now), 20);
        assert_eq!(family(waiter.request.workload), Family::Background);
        let mut disabled = config;
        disabled.aging_enabled = false;
        assert_eq!(effective_weight(&disabled, &waiter, now), 1);
    }

    #[test]
    fn smooth_weighted_fairness_and_aging_change_the_observed_share() {
        let mut weighted = HashMap::new();
        let mut weighted_counts = HashMap::new();
        for _ in 0..21 {
            let selected = choose_fair_workload(
                &mut weighted,
                &[
                    (WorkloadClass::Critical, 20, 1),
                    (WorkloadClass::Batch, 1, 2),
                ],
            )
            .unwrap();
            *weighted_counts.entry(selected).or_insert(0) += 1;
        }
        assert_eq!(weighted_counts[&WorkloadClass::Critical], 20);
        assert_eq!(weighted_counts[&WorkloadClass::Batch], 1);

        let mut aged = HashMap::new();
        let mut aged_counts = HashMap::new();
        for _ in 0..40 {
            let selected = choose_fair_workload(
                &mut aged,
                &[
                    (WorkloadClass::Critical, 20, 1),
                    (WorkloadClass::Batch, 20, 2),
                ],
            )
            .unwrap();
            *aged_counts.entry(selected).or_insert(0) += 1;
        }
        assert_eq!(aged_counts[&WorkloadClass::Critical], 20);
        assert_eq!(aged_counts[&WorkloadClass::Batch], 20);
    }

    #[tokio::test]
    async fn hard_concurrency_rejects_while_soft_concurrency_queues() {
        let scheduler = Scheduler::new(SchedulerConfig::default()).unwrap();
        let tenant = Uuid::new_v4();
        let credential = Uuid::new_v4();
        let quota_id = Uuid::new_v4();
        let mut first = request(WorkloadClass::Interactive, tenant, credential);
        first.quota.push(QuotaConcurrencyConstraint {
            quota_id,
            limit: 1,
            mode: QuotaMode::Hard,
        });
        let lease = scheduler.try_acquire(&first).unwrap().unwrap();
        assert!(matches!(
            scheduler.try_acquire(&first),
            Err(SchedulerError::HardQuota)
        ));
        assert!(matches!(
            scheduler.enqueue(first.clone()),
            Err(SchedulerError::HardQuota)
        ));

        let mut soft = first.clone();
        soft.quota[0].mode = QuotaMode::Soft;
        assert!(scheduler.try_acquire(&soft).unwrap().is_none());
        let ticket = scheduler.enqueue(soft).unwrap();
        drop(lease);
        assert!(ticket.wait().await.is_ok());
    }

    #[test]
    fn tenant_and_credential_queue_caps_are_isolated_by_family() {
        let config = SchedulerConfig {
            per_provider: 1,
            queue_per_tenant: 1,
            queue_per_credential: 1,
            ..SchedulerConfig::default()
        };
        let scheduler = Scheduler::new(config).unwrap();
        let tenant = Uuid::new_v4();
        let credential = Uuid::new_v4();
        let background = request(WorkloadClass::Background, tenant, credential);
        let _lease = scheduler.try_acquire(&background).unwrap().unwrap();
        let _background = scheduler.enqueue(background.clone()).unwrap();
        assert!(matches!(
            scheduler.enqueue(background.clone()),
            Err(SchedulerError::QueueFull)
        ));
        let mut interactive = background;
        interactive.workload = WorkloadClass::Interactive;
        assert!(scheduler.enqueue(interactive).is_ok());
    }

    #[tokio::test]
    async fn a_new_tenant_replaces_a_duplicate_share_when_its_family_queue_is_full() {
        let config = SchedulerConfig {
            queue_interactive_reserved: 1,
            queue_shared: 2,
            queue_background_reserved: 1,
            queue_per_tenant: 10,
            queue_per_credential: 10,
            per_provider: 1,
            ..SchedulerConfig::default()
        };
        let scheduler = Scheduler::new(config).unwrap();
        let provider = Uuid::new_v4();
        let tenant_a = Uuid::new_v4();
        let credential_a = Uuid::new_v4();
        let mut request_a = request(WorkloadClass::Interactive, tenant_a, credential_a);
        request_a.provider_id = provider;
        let _lease = scheduler.try_acquire(&request_a).unwrap().unwrap();
        let _first = scheduler.enqueue(request_a.clone()).unwrap();
        let _second = scheduler.enqueue(request_a.clone()).unwrap();
        let evicted = scheduler.enqueue(request_a).unwrap();

        let mut newcomer = request(WorkloadClass::Interactive, Uuid::new_v4(), Uuid::new_v4());
        newcomer.provider_id = provider;
        assert!(scheduler.enqueue(newcomer).is_ok());
        assert!(matches!(
            evicted.wait().await,
            Err(SchedulerError::QueueFull)
        ));
    }

    #[test]
    fn a_tenant_at_its_cap_cannot_evict_another_tenants_waiter() {
        let config = SchedulerConfig {
            queue_interactive_reserved: 1,
            queue_shared: 3,
            queue_background_reserved: 1,
            queue_per_tenant: 2,
            queue_per_credential: 10,
            per_provider: 1,
            ..SchedulerConfig::default()
        };
        let scheduler = Scheduler::new(config).unwrap();
        let provider = Uuid::new_v4();
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let credential_a = Uuid::new_v4();
        let credential_b = Uuid::new_v4();
        let mut request_a = request(WorkloadClass::Interactive, tenant_a, credential_a);
        request_a.provider_id = provider;
        let _lease = scheduler.try_acquire(&request_a).unwrap().unwrap();
        let _a_one = scheduler.enqueue(request_a.clone()).unwrap();
        let _a_two = scheduler.enqueue(request_a.clone()).unwrap();
        let mut request_b = request(WorkloadClass::Interactive, tenant_b, credential_b);
        request_b.provider_id = provider;
        let _b_one = scheduler.enqueue(request_b.clone()).unwrap();
        let _b_two = scheduler.enqueue(request_b).unwrap();

        request_a.credential_id = Uuid::new_v4();
        assert!(matches!(
            scheduler.enqueue(request_a),
            Err(SchedulerError::QueueFull)
        ));
        assert_eq!(scheduler.snapshot(tenant_a).unwrap()["queued"], 2);
        assert_eq!(scheduler.snapshot(tenant_b).unwrap()["queued"], 2);
    }

    #[test]
    fn cache_expiry_beats_no_affinity_only_within_one_workload() {
        let config = SchedulerConfig {
            per_user: 1,
            ..SchedulerConfig::default()
        };
        let scheduler = Scheduler::new(config).unwrap();
        let tenant = Uuid::new_v4();
        let credential = Uuid::new_v4();
        let blocker = request(WorkloadClass::Interactive, tenant, credential);
        let user_id = blocker.user_id;
        let _lease = scheduler.try_acquire(&blocker).unwrap().unwrap();
        let mut without_cache = request(WorkloadClass::Interactive, tenant, credential);
        without_cache.user_id = user_id;
        let without_provider = without_cache.provider_id;
        let _without = scheduler.enqueue(without_cache).unwrap();
        let mut cached = request(WorkloadClass::Interactive, tenant, credential);
        cached.user_id = user_id;
        cached.cache_affinity = Some(CacheAffinity {
            expires_at: Utc::now() + chrono::Duration::seconds(10),
            source: "test",
        });
        let cached_provider = cached.provider_id;
        let _cached = scheduler.enqueue(cached).unwrap();
        drop(_lease);
        let snapshot = scheduler.snapshot(tenant).unwrap();
        assert_eq!(
            snapshot["active_by_provider"][cached_provider.to_string()],
            1
        );
        assert!(
            snapshot["active_by_provider"]
                .get(without_provider.to_string())
                .is_none()
        );
    }

    #[tokio::test]
    async fn quota_and_nine_dimensions_are_released_as_one_lease() {
        let config = SchedulerConfig {
            active_interactive_reserved: 1,
            active_shared: 1,
            active_background_reserved: 1,
            per_provider: 1,
            ..SchedulerConfig::default()
        };
        let scheduler = Scheduler::new(config).unwrap();
        let tenant = Uuid::new_v4();
        let credential = Uuid::new_v4();
        let mut first = request(WorkloadClass::Interactive, tenant, credential);
        let quota_id = Uuid::new_v4();
        first.quota.push(QuotaConcurrencyConstraint {
            quota_id,
            limit: 1,
            mode: QuotaMode::Soft,
        });
        let mut second = first.clone();
        second.model_id = Uuid::new_v4();
        let lease = scheduler.try_acquire(&first).unwrap().unwrap();
        assert!(scheduler.try_acquire(&second).unwrap().is_none());
        let ticket = scheduler.enqueue(second).unwrap();
        drop(lease);
        assert!(ticket.wait().await.is_ok());
    }

    #[tokio::test]
    async fn cancelled_ticket_releases_its_isolated_queue_cell() {
        let config = SchedulerConfig {
            active_interactive_reserved: 1,
            active_shared: 1,
            active_background_reserved: 1,
            per_provider: 1,
            queue_per_tenant: 1,
            ..SchedulerConfig::default()
        };
        let scheduler = Scheduler::new(config).unwrap();
        let tenant = Uuid::new_v4();
        let credential = Uuid::new_v4();
        let first = request(WorkloadClass::Interactive, tenant, credential);
        let mut second = first.clone();
        second.model_id = Uuid::new_v4();
        let _lease = scheduler.try_acquire(&first).unwrap().unwrap();
        let ticket = scheduler.enqueue(second.clone()).unwrap();
        assert!(matches!(
            scheduler.enqueue(second.clone()),
            Err(SchedulerError::QueueFull)
        ));
        drop(ticket);
        assert!(scheduler.enqueue(second).is_ok());
    }

    #[tokio::test]
    async fn a_blocked_head_does_not_hold_an_independent_provider() {
        let config = SchedulerConfig {
            active_interactive_reserved: 1,
            active_shared: 1,
            active_background_reserved: 1,
            per_provider: 1,
            ..SchedulerConfig::default()
        };
        let scheduler = Scheduler::new(config).unwrap();
        let tenant = Uuid::new_v4();
        let credential = Uuid::new_v4();
        let first = request(WorkloadClass::Interactive, tenant, credential);
        let lease = scheduler.try_acquire(&first).unwrap().unwrap();
        let blocked = scheduler.enqueue(first.clone()).unwrap();
        let mut independent = first;
        independent.provider_id = Uuid::new_v4();
        independent.model_id = Uuid::new_v4();
        let independent = scheduler.enqueue(independent).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), independent.wait())
                .await
                .unwrap()
                .is_ok()
        );
        drop(blocked);
        drop(lease);
    }

    #[tokio::test]
    async fn close_rejects_waiters_but_active_leases_drain_normally() {
        let config = SchedulerConfig {
            active_interactive_reserved: 1,
            active_shared: 1,
            active_background_reserved: 1,
            per_provider: 1,
            ..SchedulerConfig::default()
        };
        let scheduler = Scheduler::new(config).unwrap();
        let request = request(WorkloadClass::Interactive, Uuid::new_v4(), Uuid::new_v4());
        let lease = scheduler.try_acquire(&request).unwrap().unwrap();
        let ticket = scheduler.enqueue(request.clone()).unwrap();
        scheduler.close().unwrap();
        assert!(matches!(ticket.wait().await, Err(SchedulerError::Closed)));
        drop(lease);
        assert!(matches!(
            scheduler.try_acquire(&request),
            Err(SchedulerError::Closed)
        ));
    }

    #[test]
    fn hard_deadline_rejects_without_a_real_time_sleep() {
        let scheduler = Scheduler::new(SchedulerConfig::default()).unwrap();
        let mut request = request(WorkloadClass::Interactive, Uuid::new_v4(), Uuid::new_v4());
        request.received_at = Utc::now() - chrono::Duration::minutes(1);
        assert!(matches!(
            scheduler.try_acquire(&request),
            Err(SchedulerError::Expired)
        ));
    }

    #[tokio::test]
    async fn disabling_reservations_reproduces_background_capacity_theft() {
        let config = SchedulerConfig {
            active_interactive_reserved: 1,
            active_shared: 1,
            active_background_reserved: 1,
            reservations_enabled: false,
            ..SchedulerConfig::default()
        };
        let scheduler = Scheduler::new(config).unwrap();
        let tenant = Uuid::new_v4();
        let credential = Uuid::new_v4();
        let background = request(WorkloadClass::Background, tenant, credential);
        let _one = scheduler.try_acquire(&background).unwrap().unwrap();
        let _two = scheduler.try_acquire(&background).unwrap().unwrap();
        let _three = scheduler.try_acquire(&background).unwrap().unwrap();
        let interactive = request(WorkloadClass::Interactive, tenant, credential);
        assert!(scheduler.try_acquire(&interactive).unwrap().is_none());
    }
}
