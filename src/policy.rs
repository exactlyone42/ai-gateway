use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant},
};

use axum::http::HeaderMap;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    identity::PrincipalContext, ledger::RequestPolicySnapshot, model_registry::ModelSelector,
};

const MAX_QUEUE_TIME_MS: u64 = 86_400_000;
const MAX_POLICY_DOCUMENT_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadClass {
    Critical,
    Interactive,
    Foreground,
    Automation,
    Background,
    Batch,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Idle,
    Low,
    Normal,
    Immediate,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyQuality {
    Economy,
    Standard,
    Advanced,
    Premium,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetClass {
    Cheap,
    Normal,
    Unrestricted,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyValues {
    pub workload_class: Option<WorkloadClass>,
    pub urgency: Option<Urgency>,
    pub quality: Option<PolicyQuality>,
    pub budget_class: Option<BudgetClass>,
    pub max_queue_time_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConstraints {
    pub max_quality: Option<PolicyQuality>,
    pub max_budget_class: Option<BudgetClass>,
    pub allowed_models: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDocument {
    #[serde(default)]
    pub defaults: PolicyValues,
    #[serde(default)]
    pub constraints: PolicyConstraints,
    #[serde(default)]
    pub deny: bool,
    pub deny_reason: Option<String>,
}

impl PolicyDocument {
    pub fn decode(value: Value) -> Result<Self, PolicyError> {
        if serde_json::to_vec(&value)
            .map_err(PolicyError::InvalidDocument)?
            .len()
            > MAX_POLICY_DOCUMENT_BYTES
        {
            return Err(PolicyError::InvalidValue);
        }
        let document: Self = serde_json::from_value(value).map_err(PolicyError::InvalidDocument)?;
        document.validate()?;
        Ok(document)
    }

    pub fn validate(&self) -> Result<(), PolicyError> {
        if self
            .defaults
            .max_queue_time_ms
            .is_some_and(|value| value == 0 || value > MAX_QUEUE_TIME_MS)
            || self
                .deny_reason
                .as_deref()
                .is_some_and(|reason| reason.trim().is_empty() || reason.len() > 256)
            || (!self.deny && self.deny_reason.is_some())
            || self
                .constraints
                .allowed_models
                .as_ref()
                .is_some_and(|models| {
                    models.is_empty()
                        || models.len() > 256
                        || models.iter().any(|model| {
                            model.trim() != model || model.is_empty() || model.len() > 255
                        })
                })
        {
            return Err(PolicyError::InvalidValue);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RequestHints {
    pub workload_class: Option<WorkloadClass>,
    pub urgency: Option<Urgency>,
    pub quality: Option<PolicyQuality>,
    pub budget_class: Option<BudgetClass>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub max_queue_time_ms: Option<u64>,
}

impl RequestHints {
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, PolicyError> {
        Ok(Self {
            workload_class: parse_header(headers, "x-aigw-workload-class")?,
            urgency: parse_header(headers, "x-aigw-urgency")?,
            quality: parse_header(headers, "x-aigw-quality")?,
            budget_class: parse_header(headers, "x-aigw-budget-class")?,
            deadline_at: parse_deadline(headers)?,
            max_queue_time_ms: parse_queue_time(headers)?,
        })
    }

    fn with_selector_quality(mut self, selector: &ModelSelector) -> Self {
        if self.quality.is_none() {
            self.quality = selector
                .quality()
                .and_then(|quality| parse_enum(quality).ok());
        }
        self
    }
}

#[derive(Clone, Debug)]
pub struct EffectivePolicy {
    pub workload_class: WorkloadClass,
    pub urgency: Urgency,
    pub quality: PolicyQuality,
    pub budget_class: BudgetClass,
    pub deadline_at: Option<DateTime<Utc>>,
    pub max_queue_time_ms: Option<u64>,
    pub denied: bool,
    denial_kind: Option<PolicyDenial>,
    allowed_models: Option<BTreeSet<String>>,
    decision: Value,
}

impl EffectivePolicy {
    pub fn allows_model(&self, model: &str) -> bool {
        self.allowed_models
            .as_ref()
            .is_none_or(|models| models.contains(model))
    }

    pub fn decision(&self) -> Value {
        self.decision.clone()
    }

    pub fn ledger_snapshot(&self) -> RequestPolicySnapshot {
        RequestPolicySnapshot {
            deadline_at: self.deadline_at,
            max_queue_time_ms: self
                .max_queue_time_ms
                .and_then(|value| i64::try_from(value).ok()),
            decision: self.decision(),
        }
    }

    pub fn rejection_snapshot(&self, reason: &str) -> RequestPolicySnapshot {
        let mut decision = self.decision();
        decision["denied"] = Value::Bool(true);
        decision["denial_reason"] = Value::String(reason.to_owned());
        RequestPolicySnapshot {
            deadline_at: self.deadline_at,
            max_queue_time_ms: self
                .max_queue_time_ms
                .and_then(|value| i64::try_from(value).ok()),
            decision,
        }
    }

    pub fn denial_kind(&self) -> Option<PolicyDenial> {
        self.denial_kind
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.deadline_at.is_some_and(|deadline| deadline <= now)
    }

    pub fn quality_selector(&self) -> String {
        format!("quality:{}", enum_text(&self.quality))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyDenial {
    Explicit,
    DeadlineExpired,
    ModelAllowlistEmpty,
}

#[derive(Clone)]
pub struct PolicyEngine {
    pool: PgPool,
    cache_ttl: StdDuration,
    cache: Arc<Mutex<PolicyCache>>,
}

impl PolicyEngine {
    pub fn with_cache(pool: PgPool, capacity: usize, ttl: StdDuration) -> Self {
        Self {
            pool,
            cache_ttl: ttl,
            cache: Arc::new(Mutex::new(PolicyCache::new(capacity))),
        }
    }

    pub fn clear_tenant_cache(&self, tenant_id: Uuid) {
        match self.cache.lock() {
            Ok(mut cache) => cache.clear_tenant(tenant_id),
            Err(poisoned) => poisoned.into_inner().clear_tenant(tenant_id),
        }
    }

    #[tracing::instrument(name = "policy.evaluate", skip_all)]
    pub async fn evaluate(
        &self,
        principal: &PrincipalContext,
        headers: &HeaderMap,
        selector: &ModelSelector,
        received_at: DateTime<Utc>,
    ) -> Result<EffectivePolicy, PolicyError> {
        let hints = RequestHints::from_headers(headers)?.with_selector_quality(selector);
        if principal.is_development_bootstrap() {
            return evaluate_layers(Vec::new(), hints, received_at);
        }
        if principal.tenant_id.is_nil() {
            return Err(PolicyError::InvalidTenant);
        }
        let cache_key = PolicyCacheKey::from(principal);
        let generation = {
            let mut cache = self.cache_lock()?;
            if principal.user_id.is_none()
                && let Some(rows) = cache.get(&cache_key)
            {
                return evaluate_layers(rows, hints, received_at);
            }
            cache.generation
        };
        let rows = sqlx::query_file_as!(
            PolicyRow,
            "queries/policy/evaluate.sql",
            principal.tenant_id,
            principal.user_id,
            principal.device_id,
            principal.client_id,
            principal.agent_id,
            principal.credential_id
        )
        .fetch_all(&self.pool)
        .await?;
        if principal.user_id.is_none() {
            self.cache_lock()?.insert_if_current(
                generation,
                cache_key,
                rows.clone(),
                Instant::now()
                    .checked_add(self.cache_ttl)
                    .ok_or(PolicyError::CacheUnavailable)?,
            );
        }
        evaluate_layers(rows, hints, received_at)
    }

    fn cache_lock(&self) -> Result<std::sync::MutexGuard<'_, PolicyCache>, PolicyError> {
        self.cache.lock().map_err(|_| PolicyError::CacheUnavailable)
    }
}

#[derive(Clone, Debug, FromRow)]
struct PolicyRow {
    id: Uuid,
    policy_key: String,
    revision: i64,
    configuration: Value,
    scope: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PolicyCacheKey {
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    device_id: Option<Uuid>,
    client_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    credential_id: Uuid,
}

impl From<&PrincipalContext> for PolicyCacheKey {
    fn from(principal: &PrincipalContext) -> Self {
        Self {
            tenant_id: principal.tenant_id,
            user_id: principal.user_id,
            device_id: principal.device_id,
            client_id: principal.client_id,
            agent_id: principal.agent_id,
            credential_id: principal.credential_id,
        }
    }
}

struct CachedPolicies {
    rows: Vec<PolicyRow>,
    valid_until: Instant,
}

struct PolicyCache {
    capacity: usize,
    generation: Uuid,
    entries: HashMap<PolicyCacheKey, CachedPolicies>,
    order: VecDeque<PolicyCacheKey>,
}

impl PolicyCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            generation: Uuid::new_v4(),
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &PolicyCacheKey) -> Option<Vec<PolicyRow>> {
        let valid = self
            .entries
            .get(key)
            .is_some_and(|entry| entry.valid_until > Instant::now());
        if !valid {
            self.entries.remove(key);
            self.order.retain(|candidate| candidate != key);
            return None;
        }
        self.order.retain(|candidate| candidate != key);
        self.order.push_back(key.clone());
        self.entries.get(key).map(|entry| entry.rows.clone())
    }

    fn insert(&mut self, key: PolicyCacheKey, rows: Vec<PolicyRow>, valid_until: Instant) {
        self.order.retain(|candidate| candidate != &key);
        self.entries
            .insert(key.clone(), CachedPolicies { rows, valid_until });
        self.order.push_back(key);
        while self.entries.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn insert_if_current(
        &mut self,
        generation: Uuid,
        key: PolicyCacheKey,
        rows: Vec<PolicyRow>,
        valid_until: Instant,
    ) {
        if self.generation == generation {
            self.insert(key, rows, valid_until);
        }
    }

    fn clear_tenant(&mut self, tenant_id: Uuid) {
        self.generation = Uuid::new_v4();
        self.entries.retain(|key, _| key.tenant_id != tenant_id);
        self.order.retain(|key| key.tenant_id != tenant_id);
    }
}

#[derive(Serialize)]
struct AppliedPolicy {
    id: Uuid,
    policy_key: String,
    revision: i64,
    scope: String,
    configuration_sha256: String,
}

fn evaluate_layers(
    rows: Vec<PolicyRow>,
    hints: RequestHints,
    received_at: DateTime<Utc>,
) -> Result<EffectivePolicy, PolicyError> {
    let mut values = PolicyValues {
        workload_class: Some(WorkloadClass::Interactive),
        urgency: Some(Urgency::Normal),
        quality: Some(PolicyQuality::Standard),
        budget_class: Some(BudgetClass::Normal),
        max_queue_time_ms: None,
    };
    let mut max_quality = None;
    let mut max_budget_class = None;
    let mut allowed_models: Option<BTreeSet<String>> = None;
    let mut denied = false;
    let mut denial_kind = None;
    let mut denial_reason = None;
    let mut applied = Vec::with_capacity(rows.len());

    for row in rows {
        let document = PolicyDocument::decode(row.configuration.clone())?;
        overwrite(&mut values, &document.defaults);
        max_quality = stricter(max_quality, document.constraints.max_quality);
        max_budget_class = stricter(max_budget_class, document.constraints.max_budget_class);
        if let Some(models) = document.constraints.allowed_models {
            let models = models.into_iter().collect::<BTreeSet<_>>();
            allowed_models = Some(match allowed_models {
                Some(current) => current.intersection(&models).cloned().collect(),
                None => models,
            });
        }
        if document.deny {
            denied = true;
            denial_kind.get_or_insert(PolicyDenial::Explicit);
            denial_reason.get_or_insert_with(|| {
                document
                    .deny_reason
                    .unwrap_or_else(|| "explicit_deny".to_owned())
            });
        }
        let bytes = serde_json::to_vec(&row.configuration).map_err(PolicyError::InvalidDocument)?;
        applied.push(AppliedPolicy {
            id: row.id,
            policy_key: row.policy_key,
            revision: row.revision,
            scope: row.scope,
            configuration_sha256: format!("{:x}", Sha256::digest(bytes)),
        });
    }

    let requested_quality = hints
        .quality
        .or(values.quality)
        .unwrap_or(PolicyQuality::Standard);
    let requested_budget = hints
        .budget_class
        .or(values.budget_class)
        .unwrap_or(BudgetClass::Normal);
    let quality = max_quality.map_or(requested_quality, |limit| requested_quality.min(limit));
    let budget_class =
        max_budget_class.map_or(requested_budget, |limit| requested_budget.min(limit));
    let max_queue_time_ms = minimum(hints.max_queue_time_ms, values.max_queue_time_ms);
    let queue_deadline = max_queue_time_ms
        .and_then(|value| i64::try_from(value).ok())
        .and_then(|value| received_at.checked_add_signed(Duration::milliseconds(value)));
    let deadline_at = minimum(hints.deadline_at, queue_deadline);
    if deadline_at.is_some_and(|deadline| deadline <= received_at) {
        denied = true;
        denial_kind.get_or_insert(PolicyDenial::DeadlineExpired);
        denial_reason.get_or_insert_with(|| "deadline_expired".to_owned());
    }
    if allowed_models.as_ref().is_some_and(BTreeSet::is_empty) {
        denied = true;
        denial_kind.get_or_insert(PolicyDenial::ModelAllowlistEmpty);
        denial_reason.get_or_insert_with(|| "model_allowlist_empty".to_owned());
    }

    let workload_class = hints
        .workload_class
        .or(values.workload_class)
        .unwrap_or(WorkloadClass::Interactive);
    let urgency = hints.urgency.or(values.urgency).unwrap_or(Urgency::Normal);
    let adjustments = json!({
        "quality_clamped": quality != requested_quality,
        "budget_clamped": budget_class != requested_budget,
        "deadline_expired": deadline_at.is_some_and(|deadline| deadline <= received_at),
    });
    let allowed_models_count = allowed_models.as_ref().map(BTreeSet::len);
    let allowed_models_sha256 = allowed_models.as_ref().map(|models| {
        let bytes = serde_json::to_vec(models).unwrap_or_default();
        format!("{:x}", Sha256::digest(bytes))
    });
    let decision = json!({
        "schema_version": 1,
        "applied_policies": applied,
        "request_hint": hints,
        "effective": {
            "workload_class": workload_class,
            "urgency": urgency,
            "quality": quality,
            "budget_class": budget_class,
            "deadline_at": deadline_at,
            "max_queue_time_ms": max_queue_time_ms,
        },
        "constraints": {
            "max_quality": max_quality,
            "max_budget_class": max_budget_class,
            "allowed_models_count": allowed_models_count,
            "allowed_models_sha256": allowed_models_sha256,
        },
        "adjustments": adjustments,
        "denied": denied,
        "denial_reason": denial_reason,
    });
    Ok(EffectivePolicy {
        workload_class,
        urgency,
        quality,
        budget_class,
        deadline_at,
        max_queue_time_ms,
        denied,
        denial_kind,
        allowed_models,
        decision,
    })
}

fn overwrite(current: &mut PolicyValues, next: &PolicyValues) {
    if next.workload_class.is_some() {
        current.workload_class = next.workload_class;
    }
    if next.urgency.is_some() {
        current.urgency = next.urgency;
    }
    if next.quality.is_some() {
        current.quality = next.quality;
    }
    if next.budget_class.is_some() {
        current.budget_class = next.budget_class;
    }
    if next.max_queue_time_ms.is_some() {
        current.max_queue_time_ms = next.max_queue_time_ms;
    }
}

fn stricter<T: Ord>(current: Option<T>, next: Option<T>) -> Option<T> {
    match (current, next) {
        (Some(current), Some(next)) => Some(current.min(next)),
        (current, next) => current.or(next),
    }
}

fn minimum<T: Ord>(left: Option<T>, right: Option<T>) -> Option<T> {
    stricter(left, right)
}

fn parse_header<T>(headers: &HeaderMap, name: &'static str) -> Result<Option<T>, PolicyError>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(value) = one_header(headers, name)? else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| PolicyError::InvalidHint)?;
    parse_enum(value).map(Some)
}

fn parse_enum<T>(value: &str) -> Result<T, PolicyError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(Value::String(value.to_owned())).map_err(|_| PolicyError::InvalidHint)
}

fn parse_deadline(headers: &HeaderMap) -> Result<Option<DateTime<Utc>>, PolicyError> {
    let Some(value) = one_header(headers, "x-aigw-deadline-at")? else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
        .ok_or(PolicyError::InvalidHint)
        .map(Some)
}

fn parse_queue_time(headers: &HeaderMap) -> Result<Option<u64>, PolicyError> {
    let Some(value) = one_header(headers, "x-aigw-max-queue-time-ms")? else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0 && *value <= MAX_QUEUE_TIME_MS)
        .ok_or(PolicyError::InvalidHint)
        .map(Some)
}

fn one_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<Option<&'a axum::http::HeaderValue>, PolicyError> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(PolicyError::InvalidHint);
    }
    Ok(first)
}

fn enum_text<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_default()
}

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("tenant id must not be nil")]
    InvalidTenant,
    #[error("policy document is invalid")]
    InvalidDocument(#[source] serde_json::Error),
    #[error("policy value is invalid")]
    InvalidValue,
    #[error("request policy hint is invalid")]
    InvalidHint,
    #[error("policy cache is unavailable")]
    CacheUnavailable,
    #[error("policy store is unavailable")]
    Database(#[from] sqlx::Error),
}

pub fn validate_document(value: &Value) -> Result<(), PolicyError> {
    PolicyDocument::decode(value.clone()).map(|_| ())
}

pub fn value_text<T: Serialize>(value: &T) -> String {
    enum_text(value)
}

#[cfg(test)]
pub(crate) fn effective_policy_for_deadline(
    deadline: DateTime<Utc>,
) -> Result<EffectivePolicy, PolicyError> {
    let received_at = deadline
        .checked_sub_signed(Duration::seconds(1))
        .unwrap_or(deadline);
    evaluate_layers(
        Vec::new(),
        RequestHints {
            deadline_at: Some(deadline),
            ..RequestHints::default()
        },
        received_at,
    )
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    fn row(scope: &str, revision: i64, configuration: Value) -> PolicyRow {
        PolicyRow {
            id: Uuid::new_v4(),
            policy_key: scope.to_owned(),
            revision,
            configuration,
            scope: scope.to_owned(),
        }
    }

    #[test]
    fn invalidation_prevents_inflight_lookup_from_repopulating_stale_policy() {
        let tenant_id = Uuid::new_v4();
        let key = PolicyCacheKey {
            tenant_id,
            user_id: Some(Uuid::new_v4()),
            device_id: None,
            client_id: None,
            agent_id: None,
            credential_id: Uuid::new_v4(),
        };
        let mut cache = PolicyCache::new(2);
        let generation = cache.generation;
        cache.clear_tenant(tenant_id);
        let expiry = Instant::now() + StdDuration::from_secs(60);
        cache.insert_if_current(
            generation,
            key.clone(),
            vec![row("group", 1, json!({}))],
            expiry,
        );
        assert!(cache.get(&key).is_none());
        cache.insert(key.clone(), vec![row("group", 1, json!({}))], expiry);
        assert!(cache.get(&key).is_some());
    }

    #[test]
    fn every_layer_overrides_defaults_in_declared_order() {
        let rows = [
            ("global", "batch"),
            ("user", "background"),
            ("device", "automation"),
            ("client", "foreground"),
            ("agent", "interactive"),
            ("credential", "critical"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (scope, workload))| {
            row(
                scope,
                i64::try_from(index + 1).unwrap(),
                json!({"defaults":{"workload_class":workload}}),
            )
        })
        .collect();
        let result = evaluate_layers(rows, RequestHints::default(), Utc::now()).unwrap();
        assert_eq!(result.workload_class, WorkloadClass::Critical);
        assert_eq!(
            result.decision["applied_policies"]
                .as_array()
                .unwrap()
                .len(),
            6
        );
    }

    #[test]
    fn personal_defaults_override_group_defaults_without_relaxing_group_ceiling() {
        let group = row(
            "group",
            1,
            json!({"defaults":{"workload_class":"batch"},"constraints":{"max_quality":"standard"}}),
        );
        let user = row(
            "user",
            2,
            json!({"defaults":{"workload_class":"critical","quality":"premium"}}),
        );
        let result = evaluate_layers(
            vec![group.clone(), user.clone()],
            RequestHints::default(),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(result.workload_class, WorkloadClass::Critical);
        assert_eq!(result.quality, PolicyQuality::Standard);
        let reversed =
            evaluate_layers(vec![user, group], RequestHints::default(), Utc::now()).unwrap();
        assert_eq!(reversed.workload_class, WorkloadClass::Batch);
    }

    #[test]
    fn request_hint_cannot_break_an_agent_quality_ceiling() {
        let result = evaluate_layers(
            vec![row(
                "agent",
                7,
                json!({"constraints":{"max_quality":"standard"}}),
            )],
            RequestHints {
                quality: Some(PolicyQuality::Premium),
                ..RequestHints::default()
            },
            Utc::now(),
        )
        .unwrap();
        assert_eq!(result.quality, PolicyQuality::Standard);
        assert_eq!(result.decision["adjustments"]["quality_clamped"], true);
    }

    #[test]
    fn explicit_deny_and_empty_allowlist_win() {
        let denied = evaluate_layers(
            vec![row(
                "global",
                1,
                json!({"deny":true,"deny_reason":"maintenance"}),
            )],
            RequestHints::default(),
            Utc::now(),
        )
        .unwrap();
        assert!(denied.denied);
        assert_eq!(denied.decision["denial_reason"], "maintenance");

        let empty = evaluate_layers(
            vec![
                row(
                    "global",
                    1,
                    json!({"constraints":{"allowed_models":["one"]}}),
                ),
                row(
                    "agent",
                    2,
                    json!({"constraints":{"allowed_models":["two"]}}),
                ),
            ],
            RequestHints::default(),
            Utc::now(),
        )
        .unwrap();
        assert!(empty.denied);
        assert_eq!(empty.decision["denial_reason"], "model_allowlist_empty");
    }

    #[test]
    fn queue_limit_and_request_deadline_choose_the_earliest_time() {
        let received = Utc::now();
        let result = evaluate_layers(
            vec![row(
                "global",
                1,
                json!({"defaults":{"max_queue_time_ms":5000}}),
            )],
            RequestHints {
                deadline_at: received.checked_add_signed(Duration::milliseconds(9000)),
                max_queue_time_ms: Some(3000),
                ..RequestHints::default()
            },
            received,
        )
        .unwrap();
        assert_eq!(result.max_queue_time_ms, Some(3000));
        assert_eq!(
            result.deadline_at,
            received.checked_add_signed(Duration::milliseconds(3000))
        );
    }

    #[test]
    fn request_queue_hint_cannot_relax_the_effective_policy_limit() {
        let received = Utc::now();
        let result = evaluate_layers(
            vec![
                row("global", 1, json!({"defaults":{"max_queue_time_ms":1000}})),
                row(
                    "credential",
                    2,
                    json!({"defaults":{"max_queue_time_ms":5000}}),
                ),
            ],
            RequestHints {
                max_queue_time_ms: Some(7000),
                ..RequestHints::default()
            },
            received,
        )
        .unwrap();
        assert_eq!(result.max_queue_time_ms, Some(5000));
        assert_eq!(
            result.deadline_at,
            received.checked_add_signed(Duration::milliseconds(5000))
        );
    }

    #[test]
    fn deadline_is_rechecked_against_the_provider_gate_time() {
        let received = Utc::now();
        let deadline = received
            .checked_add_signed(Duration::milliseconds(10))
            .unwrap();
        let policy = evaluate_layers(
            Vec::new(),
            RequestHints {
                deadline_at: Some(deadline),
                ..RequestHints::default()
            },
            received,
        )
        .unwrap();
        assert!(!policy.is_expired(received));
        assert!(policy.is_expired(deadline));
    }

    #[test]
    fn unknown_fields_and_unbounded_values_are_rejected() {
        assert!(PolicyDocument::decode(json!({"mystery":true})).is_err());
        assert!(PolicyDocument::decode(json!({"defaults":{"max_queue_time_ms":0}})).is_err());
        assert!(PolicyDocument::decode(json!({"deny_reason":"reason without deny"})).is_err());
    }

    #[test]
    fn source_defaults_keep_workload_urgency_quality_and_budget_independent() {
        let cases = [
            (
                "claude-code",
                "interactive",
                "immediate",
                "premium",
                "normal",
            ),
            (
                "vscode-completion",
                "interactive",
                "immediate",
                "economy",
                "cheap",
            ),
            (
                "research-agent",
                "background",
                "low",
                "premium",
                "unrestricted",
            ),
            ("memory-agent", "batch", "idle", "economy", "cheap"),
        ];
        for (source, workload, urgency, quality, budget) in cases {
            let result = evaluate_layers(
                vec![row(
                    "client",
                    1,
                    json!({"defaults": {
                        "workload_class": workload,
                        "urgency": urgency,
                        "quality": quality,
                        "budget_class": budget
                    }}),
                )],
                RequestHints::default(),
                Utc::now(),
            )
            .unwrap();
            assert_eq!(value_text(&result.workload_class), workload, "{source}");
            assert_eq!(value_text(&result.urgency), urgency, "{source}");
            assert_eq!(value_text(&result.quality), quality, "{source}");
            assert_eq!(value_text(&result.budget_class), budget, "{source}");
        }
    }

    #[test]
    fn request_headers_are_strict_and_deadlines_use_rfc3339() {
        let mut headers = HeaderMap::new();
        headers.insert("x-aigw-quality", "advanced".parse().unwrap());
        headers.insert(
            "x-aigw-deadline-at",
            "2026-09-11T08:00:00Z".parse().unwrap(),
        );
        headers.insert("x-aigw-max-queue-time-ms", "2500".parse().unwrap());
        let hints = RequestHints::from_headers(&headers).unwrap();
        assert_eq!(hints.quality, Some(PolicyQuality::Advanced));
        assert_eq!(hints.max_queue_time_ms, Some(2500));
        assert_eq!(
            hints.deadline_at.unwrap().to_rfc3339(),
            "2026-09-11T08:00:00+00:00"
        );

        headers.insert("x-aigw-quality", "best".parse().unwrap());
        assert!(matches!(
            RequestHints::from_headers(&headers),
            Err(PolicyError::InvalidHint)
        ));

        headers.remove("x-aigw-quality");
        headers.append("x-aigw-quality", "advanced".parse().unwrap());
        headers.append("x-aigw-quality", "standard".parse().unwrap());
        assert!(matches!(
            RequestHints::from_headers(&headers),
            Err(PolicyError::InvalidHint)
        ));
    }

    #[test]
    fn allowlist_snapshot_is_bounded_and_rejection_is_explicit() {
        let models = (0..256)
            .map(|index| format!("model-{index:03}"))
            .collect::<Vec<_>>();
        let policy = evaluate_layers(
            vec![row(
                "global",
                1,
                json!({"constraints":{"allowed_models":models}}),
            )],
            RequestHints::default(),
            Utc::now(),
        )
        .unwrap();
        assert!(
            policy.decision["constraints"]
                .get("allowed_models")
                .is_none()
        );
        assert_eq!(policy.decision["constraints"]["allowed_models_count"], 256);
        let rejected = policy.rejection_snapshot("model_not_allowed");
        assert_eq!(rejected.decision["denied"], true);
        assert_eq!(rejected.decision["denial_reason"], "model_not_allowed");
        assert!(serde_json::to_vec(&rejected.decision).unwrap().len() < 8192);
    }

    #[sqlx::test]
    async fn active_policy_lookup_is_tenant_scoped_and_fails_closed(pool: PgPool) {
        let tenant = Uuid::new_v4();
        let other_tenant = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO policies (
                id, tenant_id, policy_key, revision, configuration, lifecycle_state
            ) VALUES
                ($1, $2, 'tenant-default', 1, '{"defaults":{"quality":"economy"}}', 'active'),
                ($3, $4, 'other-default', 1, '{"defaults":{"quality":"premium"}}', 'active')
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(tenant)
        .bind(Uuid::new_v4())
        .bind(other_tenant)
        .execute(&pool)
        .await
        .unwrap();
        let principal = PrincipalContext {
            tenant_id: tenant,
            credential_id: Uuid::new_v4(),
            credential_name: "test".to_owned(),
            user_id: None,
            user_name: None,
            device_id: None,
            device_name: None,
            client_id: None,
            client_name: None,
            agent_id: None,
            agent_name: None,
        };
        let selector = ModelSelector::parse("company-default").unwrap();
        let result = PolicyEngine::with_cache(pool.clone(), 8, StdDuration::from_secs(60))
            .evaluate(&principal, &HeaderMap::new(), &selector, Utc::now())
            .await
            .unwrap();
        assert_eq!(result.quality, PolicyQuality::Economy);
        assert_eq!(
            result.decision["applied_policies"][0]["policy_key"],
            "tenant-default"
        );

        sqlx::query(
            "UPDATE policies SET configuration = '{\"unknown\":true}' WHERE tenant_id = $1",
        )
        .bind(tenant)
        .execute(&pool)
        .await
        .unwrap();
        let failed = PolicyEngine::with_cache(pool, 8, StdDuration::from_secs(60))
            .evaluate(&principal, &HeaderMap::new(), &selector, Utc::now())
            .await;
        assert!(matches!(failed, Err(PolicyError::InvalidDocument(_))));
    }

    #[sqlx::test]
    async fn group_deny_survives_personal_policy_and_cache_invalidation(pool: PgPool) {
        sqlx::raw_sql(include_str!("../tests/group_selection_fixture.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../tests/group_policy_deny_fixture.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let tenant = Uuid::from_u128(0x100);
        let principal = PrincipalContext {
            tenant_id: tenant,
            credential_id: Uuid::new_v4(),
            credential_name: "test".to_owned(),
            user_id: Some(Uuid::from_u128(1)),
            user_name: None,
            device_id: None,
            device_name: None,
            client_id: None,
            client_name: None,
            agent_id: None,
            agent_name: None,
        };
        let engine = PolicyEngine::with_cache(pool.clone(), 8, StdDuration::from_secs(60));
        let selector = ModelSelector::parse("company-default").unwrap();
        let first = engine
            .evaluate(&principal, &HeaderMap::new(), &selector, Utc::now())
            .await
            .unwrap();
        assert_eq!(
            first.decision["applied_policies"].as_array().unwrap().len(),
            2
        );
        assert!(first.denied);
        sqlx::raw_sql(include_str!("../tests/group_selection_user_override.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let stale = engine
            .evaluate(&principal, &HeaderMap::new(), &selector, Utc::now())
            .await
            .unwrap();
        assert_eq!(
            stale.decision["applied_policies"].as_array().unwrap().len(),
            3
        );
        assert!(stale.denied);
        engine.clear_tenant_cache(tenant);
        let refreshed = engine
            .evaluate(&principal, &HeaderMap::new(), &selector, Utc::now())
            .await
            .unwrap();
        assert_eq!(
            refreshed.decision["applied_policies"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(refreshed.denied);
        assert_eq!(
            refreshed.decision["applied_policies"][2]["policy_key"],
            "personal"
        );
    }

    #[sqlx::test]
    async fn rotated_credentials_keep_the_family_policy_and_cache_is_bounded(pool: PgPool) {
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let root = Uuid::new_v4();
        let rotated = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'user')")
            .bind(user)
            .bind(tenant)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"
            INSERT INTO credentials (
                id, tenant_id, "type", display_name, secret_hash, hash_key_id,
                key_prefix, fingerprint, user_id, rotated_from_id, rotation_root_id
            ) VALUES
                ($1, $3, 'client_api_key', 'root', decode(repeat('01', 32), 'hex'),
                 'v1', 'root', decode(repeat('02', 32), 'hex'), $4, NULL, NULL),
                ($2, $3, 'client_api_key', 'rotated', decode(repeat('03', 32), 'hex'),
                 'v1', 'rotated', decode(repeat('04', 32), 'hex'), $4, $1, $1)
            "#,
        )
        .bind(root)
        .bind(rotated)
        .bind(tenant)
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO policies (
                id, tenant_id, policy_key, revision, credential_id,
                configuration, lifecycle_state
            ) VALUES ($1, $2, 'credential-hard-limit', 1, $3,
                '{"constraints":{"max_quality":"economy"}}', 'active')
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(tenant)
        .bind(root)
        .execute(&pool)
        .await
        .unwrap();
        let principal = PrincipalContext {
            tenant_id: tenant,
            credential_id: rotated,
            credential_name: "rotated".to_owned(),
            user_id: Some(user),
            user_name: Some("user".to_owned()),
            device_id: None,
            device_name: None,
            client_id: None,
            client_name: None,
            agent_id: None,
            agent_name: None,
        };
        let engine = PolicyEngine::with_cache(pool.clone(), 1, StdDuration::from_secs(60));
        let selector = ModelSelector::parse("quality:premium").unwrap();
        let result = engine
            .evaluate(&principal, &HeaderMap::new(), &selector, Utc::now())
            .await
            .unwrap();
        assert_eq!(result.quality, PolicyQuality::Economy);

        let mut without_user = principal.clone();
        without_user.user_id = None;
        let cached_before = engine
            .evaluate(&without_user, &HeaderMap::new(), &selector, Utc::now())
            .await
            .unwrap();
        assert_eq!(cached_before.quality, PolicyQuality::Economy);

        pool.close().await;
        let cached = engine
            .evaluate(&without_user, &HeaderMap::new(), &selector, Utc::now())
            .await
            .unwrap();
        assert_eq!(cached.quality, PolicyQuality::Economy);
        assert!(matches!(
            engine
                .evaluate(&principal, &HeaderMap::new(), &selector, Utc::now())
                .await,
            Err(PolicyError::Database(_))
        ));
        engine.clear_tenant_cache(tenant);
        assert!(matches!(
            engine
                .evaluate(&without_user, &HeaderMap::new(), &selector, Utc::now())
                .await,
            Err(PolicyError::Database(_))
        ));
    }
}
