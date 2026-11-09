use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Datelike as _, Duration, TimeZone as _, Timelike as _, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Connection as _, FromRow, PgPool, Postgres, pool::PoolConnection};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    identity::PrincipalContext,
    ledger::{RequestQuotaSnapshot, TokenUsage},
    model_registry::ModelSelection,
};

pub(crate) const ADMISSION_LOCK_NAMESPACE: i64 = 12012;
const SINGLE_INSTANCE_LOCK: i64 = 12012001;
const MAX_CONFIGURATION_BYTES: usize = 16 * 1024;
const MAX_RESERVATION_AGE_HOURS: i64 = 24;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaMode {
    Hard,
    Soft,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaLimits {
    pub requests_per_minute: Option<u64>,
    pub estimated_tokens_per_minute: Option<u64>,
    pub tokens_per_hour: Option<u64>,
    pub tokens_per_day: Option<u64>,
    pub concurrent_requests: Option<u64>,
    pub cost_per_day_microusd: Option<u64>,
    pub cost_per_month_microusd: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaDocument {
    pub mode: QuotaMode,
    pub limits: QuotaLimits,
}

impl QuotaDocument {
    pub fn decode(value: Value) -> Result<Self, QuotaError> {
        if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > MAX_CONFIGURATION_BYTES) {
            return Err(QuotaError::InvalidDocument);
        }
        let document =
            serde_json::from_value::<Self>(value).map_err(|_| QuotaError::InvalidDocument)?;
        document.validate()?;
        Ok(document)
    }

    fn validate(&self) -> Result<(), QuotaError> {
        let values = [
            self.limits.requests_per_minute,
            self.limits.estimated_tokens_per_minute,
            self.limits.tokens_per_hour,
            self.limits.tokens_per_day,
            self.limits.concurrent_requests,
            self.limits.cost_per_day_microusd,
            self.limits.cost_per_month_microusd,
        ];
        if values.iter().all(Option::is_none)
            || values
                .into_iter()
                .flatten()
                .any(|value| value > i64::MAX as u64)
        {
            return Err(QuotaError::InvalidDocument);
        }
        Ok(())
    }

    fn has_cost_limit(&self) -> bool {
        self.limits.cost_per_day_microusd.is_some() || self.limits.cost_per_month_microusd.is_some()
    }
}

pub fn validate_document(value: &Value) -> Result<(), QuotaError> {
    QuotaDocument::decode(value.clone()).map(|_| ())
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenPricing {
    schema: PricingSchema,
    currency: Currency,
    input_microusd_per_million_tokens: u64,
    output_microusd_per_million_tokens: u64,
    cache_read_input_microusd_per_million_tokens: Option<u64>,
    cache_write_input_microusd_per_million_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
enum PricingSchema {
    #[serde(rename = "token_v1")]
    TokenV1,
}

#[derive(Clone, Copy, Debug, Deserialize)]
enum Currency {
    #[serde(rename = "USD")]
    Usd,
}

#[derive(Clone)]
pub struct QuotaEngine {
    pool: PgPool,
    singleton_guard: Arc<tokio::sync::Mutex<Option<PoolConnection<Postgres>>>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct QuotaConcurrencyConstraint {
    pub quota_id: Uuid,
    pub limit: u64,
    pub mode: QuotaMode,
}

pub struct QuotaAdmission {
    pub selection: ModelSelection,
    pub estimated_usage: TokenUsage,
    pub snapshot: Option<RequestQuotaSnapshot>,
    pub concurrency: Vec<QuotaConcurrencyConstraint>,
}

#[derive(Clone, Copy)]
pub struct QuotaEstimate {
    pub input_upper_bound: u64,
    pub requested_output_tokens: Option<u64>,
    pub output_multiplier: u64,
    pub billable: bool,
    pub unbounded_input: bool,
    pub requires_extended_pricing: bool,
    pub unsupported_cost_features: bool,
}

pub enum QuotaOutcome {
    Allowed(Box<QuotaAdmission>),
    Rejected(RequestQuotaSnapshot, QuotaRejection),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuotaRejection {
    HardLimit,
    SoftDeferred,
}

#[derive(Debug, Error)]
pub enum QuotaError {
    #[error("quota configuration is invalid")]
    InvalidDocument,
    #[error("cost quota requires complete USD token pricing and an output cap")]
    CostUnavailable,
    #[error("another gateway instance already owns the single-instance quota lock")]
    MultipleInstances,
    #[error("quota database is unavailable")]
    Database(#[from] sqlx::Error),
}

#[derive(Clone, FromRow)]
struct QuotaRow {
    id: Uuid,
    quota_key: String,
    revision: i64,
    model_id: Option<Uuid>,
    provider_id: Option<Uuid>,
    configuration: Value,
}

struct ResolvedQuota {
    row: QuotaRow,
    document: QuotaDocument,
}

#[derive(FromRow)]
struct WindowRow {
    quota_id: Uuid,
    limit_kind: String,
    window_start: DateTime<Utc>,
    reserved_requests: i64,
    reserved_tokens: i64,
    reserved_cost_microusd: i64,
}

#[derive(Clone, Serialize)]
struct Allocation {
    quota_id: Uuid,
    quota_key: String,
    quota_revision: i64,
    limit_kind: String,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    reserved_requests: i64,
    reserved_tokens: i64,
    reserved_cost_microusd: i64,
}

struct CandidateEvaluation {
    allocations: Vec<Allocation>,
    hard: Vec<Value>,
    soft: Vec<Value>,
    concurrency: Vec<QuotaConcurrencyConstraint>,
    estimated_usage: TokenUsage,
    estimated_cost_microusd: u64,
    requires_cost: bool,
}

enum CandidateDecision {
    Selected(usize, CandidateEvaluation),
    Rejected(RequestQuotaSnapshot, QuotaRejection),
    Missing,
}

impl QuotaEngine {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            singleton_guard: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    pub async fn acquire_single_instance_guard(&self) -> Result<(), QuotaError> {
        let mut guard = self.singleton_guard.lock().await;
        if let Some(connection) = guard.as_mut() {
            connection.ping().await?;
            return Ok(());
        }
        let mut connection = self.pool.acquire().await?;
        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(SINGLE_INSTANCE_LOCK)
            .fetch_one(&mut *connection)
            .await?;
        if !acquired {
            return Err(QuotaError::MultipleInstances);
        }
        *guard = Some(connection);
        Ok(())
    }

    pub async fn verify_single_instance_guard(&self) -> Result<(), QuotaError> {
        let mut guard = self.singleton_guard.lock().await;
        let connection = guard.as_mut().ok_or(QuotaError::MultipleInstances)?;
        connection.ping().await?;
        Ok(())
    }

    pub async fn wait_for_single_instance_guard_loss(&self) {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if !matches!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    self.verify_single_instance_guard()
                )
                .await,
                Ok(Ok(()))
            ) {
                return;
            }
        }
    }

    #[tracing::instrument(name = "quota.admit", skip_all)]
    #[cfg(test)]
    pub async fn admit(
        &self,
        gateway_request_id: Uuid,
        principal: &PrincipalContext,
        candidates: Vec<ModelSelection>,
        estimate: QuotaEstimate,
        at: DateTime<Utc>,
    ) -> Result<QuotaOutcome, QuotaError> {
        self.admit_with_soft_routing(
            gateway_request_id,
            principal,
            candidates,
            estimate,
            at,
            true,
        )
        .await
    }

    pub async fn admit_with_soft_routing(
        &self,
        gateway_request_id: Uuid,
        principal: &PrincipalContext,
        candidates: Vec<ModelSelection>,
        estimate: QuotaEstimate,
        at: DateTime<Utc>,
        route_on_soft_limit: bool,
    ) -> Result<QuotaOutcome, QuotaError> {
        let input_upper_bound = estimate.input_upper_bound;
        let requested_output_tokens = estimate.requested_output_tokens;
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, $2))")
            .bind(principal.tenant_id)
            .bind(ADMISSION_LOCK_NAMESPACE)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "UPDATE quota_reservations SET state = 'expired' WHERE tenant_id = $1 AND state = 'active' AND expires_at <= $2",
        )
        .bind(principal.tenant_id)
        .bind(at)
        .execute(&mut *transaction)
        .await?;

        let model_ids = candidates
            .iter()
            .map(|value| value.model_id)
            .collect::<Vec<_>>();
        let provider_ids = candidates
            .iter()
            .map(|value| value.provider_id)
            .collect::<Vec<_>>();
        let rows = sqlx::query_file_as!(
            QuotaRow,
            "queries/quota/active.sql",
            principal.tenant_id,
            principal.user_id,
            principal.device_id,
            principal.client_id,
            principal.agent_id,
            principal.credential_id,
            &model_ids,
            &provider_ids
        )
        .fetch_all(&mut *transaction)
        .await?;
        let quotas = rows
            .into_iter()
            .map(|row| {
                let document = QuotaDocument::decode(row.configuration.clone())?;
                Ok(ResolvedQuota { row, document })
            })
            .collect::<Result<Vec<_>, QuotaError>>()?;

        let mut candidates = candidates.into_iter();
        let Some(first) = candidates.next() else {
            return Err(QuotaError::InvalidDocument);
        };
        let mut candidates = std::iter::once(first).chain(candidates).collect::<Vec<_>>();
        if quotas.is_empty() {
            transaction.commit().await?;
            let selection = candidates.remove(0);
            return Ok(QuotaOutcome::Allowed(Box::new(QuotaAdmission {
                selection,
                estimated_usage: TokenUsage {
                    input_tokens: input_upper_bound,
                    output_tokens: requested_output_tokens.unwrap_or(0),
                    ..TokenUsage::default()
                },
                snapshot: None,
                concurrency: Vec::new(),
            })));
        }

        let quota_ids = quotas.iter().map(|quota| quota.row.id).collect::<Vec<_>>();
        let windows = sqlx::query_as::<_, WindowRow>(
            r#"
            SELECT quota_id, limit_kind, window_start,
                   reserved_requests, reserved_tokens, reserved_cost_microusd
            FROM quota_windows
            WHERE tenant_id = $1 AND quota_id = ANY($2) AND window_start <= $3 AND window_end > $3
            "#,
        )
        .bind(principal.tenant_id)
        .bind(&quota_ids)
        .bind(at)
        .fetch_all(&mut *transaction)
        .await?
        .into_iter()
        .map(|row| {
            (
                (row.quota_id, row.limit_kind.clone(), row.window_start),
                row,
            )
        })
        .collect::<HashMap<_, _>>();

        let mut skipped_soft = Vec::new();
        let decision = {
            let mut decision = CandidateDecision::Missing;
            for (index, candidate) in candidates.iter().enumerate() {
                let evaluation = evaluate_candidate(&quotas, &windows, candidate, estimate, at)?;
                if !evaluation.hard.is_empty() {
                    decision = CandidateDecision::Rejected(
                        rejection_snapshot(
                            "hard_limit",
                            candidate,
                            &evaluation.hard,
                            &skipped_soft,
                        ),
                        QuotaRejection::HardLimit,
                    );
                    break;
                }
                if route_on_soft_limit
                    && !evaluation.soft.is_empty()
                    && index + 1 < candidates.len()
                {
                    skipped_soft.extend(evaluation.soft);
                    continue;
                }
                if !evaluation.soft.is_empty() {
                    decision = CandidateDecision::Rejected(
                        rejection_snapshot(
                            "soft_limit_deferred",
                            candidate,
                            &evaluation.soft,
                            &skipped_soft,
                        ),
                        QuotaRejection::SoftDeferred,
                    );
                    break;
                }
                decision = CandidateDecision::Selected(index, evaluation);
                break;
            }
            decision
        };
        let (index, evaluation) = match decision {
            CandidateDecision::Selected(index, evaluation) => (index, evaluation),
            CandidateDecision::Rejected(snapshot, rejection) => {
                transaction.rollback().await?;
                return Ok(QuotaOutcome::Rejected(snapshot, rejection));
            }
            CandidateDecision::Missing => {
                transaction.rollback().await?;
                return Err(QuotaError::InvalidDocument);
            }
        };
        let selection = candidates.remove(index);
        let reservation_id = Uuid::new_v4();
        let decision = json!({
            "schema_version": 1,
            "reservation_id": reservation_id,
            "selected_model_id": selection.model_id,
            "selected_provider_id": selection.provider_id,
            "selected_quality": selection.selector.quality(),
            "soft_limits_causing_downgrade": skipped_soft,
            "soft_limits_at_floor": evaluation.soft,
            "concurrency": evaluation.concurrency,
            "applied": evaluation.allocations.iter().map(|allocation| json!({
                "quota_id": allocation.quota_id,
                "quota_key": allocation.quota_key,
                "revision": allocation.quota_revision,
                "limit_kind": allocation.limit_kind,
                "window_start": allocation.window_start,
                "window_end": allocation.window_end,
            })).collect::<Vec<_>>(),
        });
        let allocations = serde_json::to_value(&evaluation.allocations)
            .map_err(|_| QuotaError::InvalidDocument)?;
        let estimated_tokens = total_tokens(&evaluation.estimated_usage)?;
        sqlx::query(
            r#"
            INSERT INTO quota_windows (
                tenant_id, quota_id, limit_kind, window_start, window_end,
                reserved_requests, reserved_tokens, reserved_cost_microusd
            )
            SELECT $1, item.quota_id, item.limit_kind, item.window_start, item.window_end,
                   item.reserved_requests, item.reserved_tokens, item.reserved_cost_microusd
            FROM jsonb_to_recordset($2) AS item(
                quota_id UUID, limit_kind TEXT, window_start TIMESTAMPTZ, window_end TIMESTAMPTZ,
                reserved_requests BIGINT, reserved_tokens BIGINT, reserved_cost_microusd BIGINT
            )
            ON CONFLICT (tenant_id, quota_id, limit_kind, window_start) DO UPDATE SET
                reserved_requests = quota_windows.reserved_requests + EXCLUDED.reserved_requests,
                reserved_tokens = quota_windows.reserved_tokens + EXCLUDED.reserved_tokens,
                reserved_cost_microusd = quota_windows.reserved_cost_microusd + EXCLUDED.reserved_cost_microusd,
                updated_at = now()
            "#,
        )
        .bind(principal.tenant_id)
        .bind(&allocations)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO quota_reservations (
                id, tenant_id, gateway_request_id, state, estimated_tokens,
                estimated_cost_microusd, decision, allocations, expires_at
            ) VALUES ($1, $2, $3, 'active', $4, $5, $6, $7, $8)
            "#,
        )
        .bind(reservation_id)
        .bind(principal.tenant_id)
        .bind(gateway_request_id)
        .bind(to_i64(estimated_tokens)?)
        .bind(to_i64(evaluation.estimated_cost_microusd)?)
        .bind(&decision)
        .bind(&allocations)
        .bind(at + Duration::hours(MAX_RESERVATION_AGE_HOURS))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(QuotaOutcome::Allowed(Box::new(QuotaAdmission {
            selection,
            estimated_usage: evaluation.estimated_usage,
            snapshot: Some(RequestQuotaSnapshot {
                reservation_id: Some(reservation_id),
                decision,
                requires_cost: evaluation.requires_cost,
            }),
            concurrency: evaluation.concurrency,
        })))
    }
}

fn evaluate_candidate(
    quotas: &[ResolvedQuota],
    windows: &HashMap<(Uuid, String, DateTime<Utc>), WindowRow>,
    candidate: &ModelSelection,
    estimate: QuotaEstimate,
    at: DateTime<Utc>,
) -> Result<CandidateEvaluation, QuotaError> {
    let QuotaEstimate {
        input_upper_bound,
        requested_output_tokens,
        output_multiplier,
        billable,
        unbounded_input,
        requires_extended_pricing,
        unsupported_cost_features,
    } = estimate;
    let applicable = quotas
        .iter()
        .filter(|quota| applies(&quota.row, candidate))
        .collect::<Vec<_>>();
    let has_cost_limit = billable
        && applicable
            .iter()
            .any(|quota| quota.document.has_cost_limit());
    let has_token_limit = billable
        && applicable.iter().any(|quota| {
            quota.document.limits.estimated_tokens_per_minute.is_some()
                || quota.document.limits.tokens_per_hour.is_some()
                || quota.document.limits.tokens_per_day.is_some()
        });
    let needs_usage_bound = billable && (has_cost_limit || has_token_limit);
    let output_tokens = if needs_usage_bound {
        if unbounded_input {
            return Err(QuotaError::CostUnavailable);
        }
        requested_output_tokens
            .or_else(|| {
                candidate
                    .max_output_tokens
                    .and_then(|value| u64::try_from(value).ok())
            })
            .ok_or(QuotaError::CostUnavailable)?
            .checked_mul(output_multiplier)
            .ok_or(QuotaError::CostUnavailable)?
    } else {
        0
    };
    let estimated_usage = TokenUsage {
        input_tokens: if billable { input_upper_bound } else { 0 },
        output_tokens,
        ..TokenUsage::default()
    };
    let estimated_cost_microusd = if has_cost_limit {
        if unsupported_cost_features {
            return Err(QuotaError::CostUnavailable);
        }
        if requires_extended_pricing {
            cost_for_cached_upper_bound(&candidate.cost_metadata, &estimated_usage)?
        } else {
            cost_for_usage(&candidate.cost_metadata, &estimated_usage)?
        }
    } else {
        0
    };
    let estimated_tokens = total_tokens(&estimated_usage)?;
    let mut allocations = Vec::new();
    let mut hard = Vec::new();
    let mut soft = Vec::new();
    let mut concurrency = Vec::new();
    for quota in applicable {
        for (kind, limit, _amount) in persistent_limits(
            &quota.document.limits,
            estimated_tokens,
            estimated_cost_microusd,
        )
        .into_iter()
        .filter(|(kind, _, _)| billable || *kind == "requests_per_minute")
        {
            let (start, end) = window_bounds(kind, at)?;
            let current = windows.get(&(quota.row.id, kind.to_owned(), start));
            let (requests, tokens, cost) = match kind {
                "requests_per_minute" => (1, 0, 0),
                "estimated_tokens_per_minute" | "tokens_per_hour" | "tokens_per_day" => {
                    (0, to_i64(estimated_tokens)?, 0)
                }
                "cost_per_day_microusd" | "cost_per_month_microusd" => {
                    (0, 0, to_i64(estimated_cost_microusd)?)
                }
                _ => return Err(QuotaError::InvalidDocument),
            };
            let used = current.map_or(0, |row| match kind {
                "requests_per_minute" => row.reserved_requests,
                "estimated_tokens_per_minute" | "tokens_per_hour" | "tokens_per_day" => {
                    row.reserved_tokens
                }
                _ => row.reserved_cost_microusd,
            });
            let next = used
                .checked_add(requests.max(tokens).max(cost))
                .ok_or(QuotaError::InvalidDocument)?;
            if next > to_i64(limit)? {
                violation(&mut hard, &mut soft, quota, kind, limit, used);
            }
            allocations.push(Allocation {
                quota_id: quota.row.id,
                quota_key: quota.row.quota_key.clone(),
                quota_revision: quota.row.revision,
                limit_kind: kind.to_owned(),
                window_start: start,
                window_end: end,
                reserved_requests: requests,
                reserved_tokens: tokens,
                reserved_cost_microusd: cost,
            });
        }
        if let Some(limit) = quota.document.limits.concurrent_requests {
            concurrency.push(QuotaConcurrencyConstraint {
                quota_id: quota.row.id,
                limit,
                mode: quota.document.mode,
            });
        }
    }
    Ok(CandidateEvaluation {
        allocations,
        hard,
        soft,
        concurrency,
        estimated_usage,
        estimated_cost_microusd,
        requires_cost: has_cost_limit,
    })
}

fn persistent_limits(
    limits: &QuotaLimits,
    tokens: u64,
    cost: u64,
) -> Vec<(&'static str, u64, u64)> {
    [
        ("requests_per_minute", limits.requests_per_minute, 1),
        (
            "estimated_tokens_per_minute",
            limits.estimated_tokens_per_minute,
            tokens,
        ),
        ("tokens_per_hour", limits.tokens_per_hour, tokens),
        ("tokens_per_day", limits.tokens_per_day, tokens),
        ("cost_per_day_microusd", limits.cost_per_day_microusd, cost),
        (
            "cost_per_month_microusd",
            limits.cost_per_month_microusd,
            cost,
        ),
    ]
    .into_iter()
    .filter_map(|(kind, limit, amount)| limit.map(|limit| (kind, limit, amount)))
    .collect()
}

fn violation(
    hard: &mut Vec<Value>,
    soft: &mut Vec<Value>,
    quota: &ResolvedQuota,
    kind: &str,
    limit: u64,
    used: i64,
) {
    let value = json!({
        "quota_id": quota.row.id,
        "quota_key": quota.row.quota_key,
        "revision": quota.row.revision,
        "limit_kind": kind,
        "limit": limit,
        "used": used,
    });
    match quota.document.mode {
        QuotaMode::Hard => hard.push(value),
        QuotaMode::Soft => soft.push(value),
    }
}

fn applies(quota: &QuotaRow, candidate: &ModelSelection) -> bool {
    quota.model_id.is_none_or(|id| id == candidate.model_id)
        && quota
            .provider_id
            .is_none_or(|id| id == candidate.provider_id)
}

fn rejection_snapshot(
    reason: &str,
    candidate: &ModelSelection,
    violations: &[Value],
    prior_soft: &[Value],
) -> RequestQuotaSnapshot {
    RequestQuotaSnapshot {
        reservation_id: None,
        decision: json!({
            "schema_version": 1,
            "denied": true,
            "denial_reason": reason,
            "selected_model_id": candidate.model_id,
            "selected_provider_id": candidate.provider_id,
            "violations": violations,
            "prior_soft_violations": prior_soft,
        }),
        requires_cost: false,
    }
}

fn window_bounds(
    kind: &str,
    at: DateTime<Utc>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), QuotaError> {
    let day = Utc
        .with_ymd_and_hms(at.year(), at.month(), at.day(), 0, 0, 0)
        .single()
        .ok_or(QuotaError::InvalidDocument)?;
    match kind {
        "requests_per_minute" | "estimated_tokens_per_minute" => {
            let start = at
                .with_second(0)
                .and_then(|value| value.with_nanosecond(0))
                .ok_or(QuotaError::InvalidDocument)?;
            Ok((start, start + Duration::minutes(1)))
        }
        "tokens_per_hour" => {
            let start = at
                .with_minute(0)
                .and_then(|value| value.with_second(0))
                .and_then(|value| value.with_nanosecond(0))
                .ok_or(QuotaError::InvalidDocument)?;
            Ok((start, start + Duration::hours(1)))
        }
        "tokens_per_day" | "cost_per_day_microusd" => Ok((day, day + Duration::days(1))),
        "cost_per_month_microusd" => {
            let start = Utc
                .with_ymd_and_hms(at.year(), at.month(), 1, 0, 0, 0)
                .single()
                .ok_or(QuotaError::InvalidDocument)?;
            let (year, month) = if at.month() == 12 {
                (at.year() + 1, 1)
            } else {
                (at.year(), at.month() + 1)
            };
            let end = Utc
                .with_ymd_and_hms(year, month, 1, 0, 0, 0)
                .single()
                .ok_or(QuotaError::InvalidDocument)?;
            Ok((start, end))
        }
        _ => Err(QuotaError::InvalidDocument),
    }
}

pub fn cost_for_usage(metadata: &Value, usage: &TokenUsage) -> Result<u64, QuotaError> {
    let pricing = serde_json::from_value::<TokenPricing>(metadata.clone())
        .map_err(|_| QuotaError::CostUnavailable)?;
    let _ = (pricing.schema, pricing.currency);
    let input = u128::from(usage.input_tokens)
        .checked_mul(u128::from(pricing.input_microusd_per_million_tokens))
        .ok_or(QuotaError::CostUnavailable)?;
    let output = u128::from(usage.output_tokens)
        .checked_mul(u128::from(pricing.output_microusd_per_million_tokens))
        .ok_or(QuotaError::CostUnavailable)?;
    let cache_read = priced_optional(
        usage.cached_input_tokens,
        pricing.cache_read_input_microusd_per_million_tokens,
    )?;
    let cache_write = priced_optional(
        usage.cache_write_input_tokens,
        pricing.cache_write_input_microusd_per_million_tokens,
    )?;
    let numerator = input
        .checked_add(output)
        .and_then(|value| value.checked_add(cache_read))
        .and_then(|value| value.checked_add(cache_write))
        .ok_or(QuotaError::CostUnavailable)?;
    let rounded = numerator
        .checked_add(999_999)
        .ok_or(QuotaError::CostUnavailable)?
        / 1_000_000;
    u64::try_from(rounded).map_err(|_| QuotaError::CostUnavailable)
}

fn cost_for_cached_upper_bound(metadata: &Value, usage: &TokenUsage) -> Result<u64, QuotaError> {
    let pricing = serde_json::from_value::<TokenPricing>(metadata.clone())
        .map_err(|_| QuotaError::CostUnavailable)?;
    let cache_read = pricing
        .cache_read_input_microusd_per_million_tokens
        .ok_or(QuotaError::CostUnavailable)?;
    let cache_write = pricing
        .cache_write_input_microusd_per_million_tokens
        .ok_or(QuotaError::CostUnavailable)?;
    let input_rate = [
        cache_read,
        cache_write,
        pricing.input_microusd_per_million_tokens,
    ]
    .into_iter()
    .max()
    .ok_or(QuotaError::CostUnavailable)?;
    let bounded = TokenUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        ..TokenUsage::default()
    };
    let metadata = json!({
        "schema": "token_v1",
        "currency": "USD",
        "input_microusd_per_million_tokens": input_rate,
        "output_microusd_per_million_tokens": pricing.output_microusd_per_million_tokens,
    });
    cost_for_usage(&metadata, &bounded)
}

fn priced_optional(tokens: u64, rate: Option<u64>) -> Result<u128, QuotaError> {
    if tokens == 0 {
        return Ok(0);
    }
    u128::from(tokens)
        .checked_mul(u128::from(rate.ok_or(QuotaError::CostUnavailable)?))
        .ok_or(QuotaError::CostUnavailable)
}

pub fn total_tokens(usage: &TokenUsage) -> Result<u64, QuotaError> {
    [
        usage.input_tokens,
        usage.output_tokens,
        usage.cached_input_tokens,
        usage.cache_write_input_tokens,
        usage.reasoning_tokens,
    ]
    .into_iter()
    .try_fold(0_u64, |total, value| total.checked_add(value))
    .ok_or(QuotaError::InvalidDocument)
}

fn to_i64(value: u64) -> Result<i64, QuotaError> {
    i64::try_from(value).map_err(|_| QuotaError::InvalidDocument)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlx::PgPool;

    use super::*;
    use crate::model_registry::{
        GatewayOperation, ModelSelector, ProviderDialect, ProviderProfile,
    };

    fn principal(tenant_id: Uuid) -> PrincipalContext {
        PrincipalContext {
            tenant_id,
            credential_id: Uuid::nil(),
            credential_name: "quota test".to_owned(),
            user_id: None,
            user_name: None,
            device_id: None,
            device_name: None,
            client_id: None,
            client_name: None,
            agent_id: None,
            agent_name: None,
        }
    }

    fn selection(tenant_id: Uuid, selector: &str, model: Uuid, provider: Uuid) -> ModelSelection {
        ModelSelection {
            selector: ModelSelector::parse(selector).unwrap(),
            alias_id: selector.starts_with("quality:").then(Uuid::new_v4),
            alias_version: selector.starts_with("quality:").then_some(1),
            model_id: model,
            model_version: 1,
            public_model_id: selector.to_owned(),
            provider_model_id: selector.to_owned(),
            provider_id: provider,
            provider_version: 1,
            provider_key: "quota-provider".to_owned(),
            description: None,
            context_window: None,
            latency_class: None,
            supports_streaming: false,
            supports_tools: false,
            supports_reasoning: false,
            supports_images: false,
            supports_structured_output: false,
            max_output_tokens: Some(4096),
            cost_metadata: json!({
                "schema": "token_v1",
                "currency": "USD",
                "input_microusd_per_million_tokens": 1000,
                "output_microusd_per_million_tokens": 2000
            }),
            profile: ProviderProfile::bearer(
                "env://QUOTA_TEST_KEY",
                "quota-provider",
                "https://quota.invalid",
                ProviderDialect::OpenAi,
                vec![GatewayOperation::OpenAiResponses],
                vec![tenant_id],
                Arc::<str>::from("secret"),
            ),
            model_supported_operations: vec![GatewayOperation::OpenAiResponses],
            provider_supported_operations: vec![GatewayOperation::OpenAiResponses],
        }
    }

    async fn insert_global_quota(pool: &PgPool, tenant_id: Uuid, document: Value) {
        sqlx::query(
            "INSERT INTO quotas (id, tenant_id, quota_key, revision, configuration, lifecycle_state) VALUES ($1, $2, 'global', 1, $3, 'active')",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(document)
        .execute(pool)
        .await
        .unwrap();
    }

    #[test]
    fn quota_documents_are_typed_bounded_and_allow_zero_for_a_closed_gate() {
        assert!(
            QuotaDocument::decode(json!({
                "mode": "hard",
                "limits": {"requests_per_minute": 0}
            }))
            .is_ok()
        );
        assert!(QuotaDocument::decode(json!({"mode": "hard", "limits": {}})).is_err());
        assert!(
            QuotaDocument::decode(json!({
                "mode": "hard",
                "limits": {"requests": 10}
            }))
            .is_err()
        );
    }

    #[test]
    fn windows_are_utc_and_cross_day_month_without_local_time() {
        let at = Utc.with_ymd_and_hms(2028, 3, 31, 23, 59, 59).unwrap();
        let (day, day_end) = window_bounds("tokens_per_day", at).unwrap();
        assert_eq!(day.to_rfc3339(), "2028-03-31T00:00:00+00:00");
        assert_eq!(day_end.to_rfc3339(), "2028-04-01T00:00:00+00:00");
        let (month, month_end) = window_bounds("cost_per_month_microusd", at).unwrap();
        assert_eq!(month.to_rfc3339(), "2028-03-01T00:00:00+00:00");
        assert_eq!(month_end.to_rfc3339(), "2028-04-01T00:00:00+00:00");
    }

    #[test]
    fn token_cost_uses_integer_microusd_and_rounds_up() {
        let cost = cost_for_usage(
            &json!({
                "schema": "token_v1",
                "currency": "USD",
                "input_microusd_per_million_tokens": 2_000_000,
                "output_microusd_per_million_tokens": 8_000_000
            }),
            &TokenUsage {
                input_tokens: 1,
                output_tokens: 1,
                ..TokenUsage::default()
            },
        )
        .unwrap();
        assert_eq!(cost, 10);
    }

    #[sqlx::test]
    async fn a_zero_hard_limit_rejects_with_the_exact_limit(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        insert_global_quota(
            &pool,
            tenant_id,
            json!({"mode": "hard", "limits": {"requests_per_minute": 0}}),
        )
        .await;
        let outcome = QuotaEngine::new(pool)
            .admit(
                Uuid::new_v4(),
                &principal(tenant_id),
                vec![selection(
                    tenant_id,
                    "direct-model",
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                )],
                QuotaEstimate {
                    input_upper_bound: 10,
                    requested_output_tokens: Some(10),
                    output_multiplier: 1,
                    billable: true,
                    unbounded_input: false,
                    requires_extended_pricing: false,
                    unsupported_cost_features: false,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let QuotaOutcome::Rejected(snapshot, QuotaRejection::HardLimit) = outcome else {
            panic!("zero hard quota did not reject");
        };
        assert_eq!(snapshot.decision["denial_reason"], "hard_limit");
        assert_eq!(
            snapshot.decision["violations"][0]["limit_kind"],
            "requests_per_minute"
        );
    }

    #[sqlx::test]
    async fn personal_quota_replaces_matching_group_limit_during_admission(pool: PgPool) {
        sqlx::raw_sql(include_str!("../tests/group_selection_fixture.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../tests/group_quota_limits_fixture.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let tenant = Uuid::from_u128(0x100);
        let mut identity = principal(tenant);
        identity.user_id = Some(Uuid::from_u128(1));
        let engine = QuotaEngine::new(pool.clone());
        let estimate = QuotaEstimate {
            input_upper_bound: 10,
            requested_output_tokens: Some(10),
            output_multiplier: 1,
            billable: true,
            unbounded_input: false,
            requires_extended_pricing: false,
            unsupported_cost_features: false,
        };
        let candidate = selection(tenant, "direct-model", Uuid::new_v4(), Uuid::new_v4());
        let first = engine
            .admit(
                Uuid::new_v4(),
                &identity,
                vec![candidate.clone()],
                estimate,
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(matches!(
            first,
            QuotaOutcome::Rejected(_, QuotaRejection::HardLimit)
        ));
        sqlx::raw_sql(include_str!("../tests/group_quota_personal_fixture.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query_file!("tests/group_quota_second_limit.sql", 0_i32)
            .execute(&pool)
            .await
            .unwrap();
        let other_group = engine
            .admit(
                Uuid::new_v4(),
                &identity,
                vec![candidate.clone()],
                estimate,
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(matches!(
            other_group,
            QuotaOutcome::Rejected(_, QuotaRejection::HardLimit)
        ));
        sqlx::query_file!("tests/group_quota_second_limit.sql", 100_i32)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query_file!("tests/personal_quota_key.sql", "different")
            .execute(&pool)
            .await
            .unwrap();
        let mismatched = engine
            .admit(
                Uuid::new_v4(),
                &identity,
                vec![candidate.clone()],
                estimate,
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(matches!(
            mismatched,
            QuotaOutcome::Rejected(_, QuotaRejection::HardLimit)
        ));
        sqlx::query_file!("tests/personal_quota_key.sql", "first")
            .execute(&pool)
            .await
            .unwrap();
        let second = engine
            .admit(
                Uuid::new_v4(),
                &identity,
                vec![candidate],
                estimate,
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(matches!(second, QuotaOutcome::Allowed(_)));
    }

    #[sqlx::test]
    async fn missing_output_cap_uses_model_maximum_and_chat_choice_multiplier(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        insert_global_quota(
            &pool,
            tenant_id,
            json!({"mode": "hard", "limits": {"tokens_per_hour": 8_000}}),
        )
        .await;
        let outcome = QuotaEngine::new(pool)
            .admit(
                Uuid::new_v4(),
                &principal(tenant_id),
                vec![selection(
                    tenant_id,
                    "direct-model",
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                )],
                QuotaEstimate {
                    input_upper_bound: 10,
                    requested_output_tokens: None,
                    output_multiplier: 2,
                    billable: true,
                    unbounded_input: false,
                    requires_extended_pricing: false,
                    unsupported_cost_features: false,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            QuotaOutcome::Rejected(_, QuotaRejection::HardLimit)
        ));
    }

    #[sqlx::test]
    async fn token_counting_uses_only_request_rate_quota(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        insert_global_quota(
            &pool,
            tenant_id,
            json!({"mode": "hard", "limits": {
                "requests_per_minute": 1,
                "tokens_per_hour": 0,
                "cost_per_day_microusd": 0
            }}),
        )
        .await;
        let outcome = QuotaEngine::new(pool)
            .admit(
                Uuid::new_v4(),
                &principal(tenant_id),
                vec![selection(
                    tenant_id,
                    "direct-model",
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                )],
                QuotaEstimate {
                    input_upper_bound: 0,
                    requested_output_tokens: Some(0),
                    output_multiplier: 1,
                    billable: false,
                    unbounded_input: false,
                    requires_extended_pricing: false,
                    unsupported_cost_features: false,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let QuotaOutcome::Allowed(admission) = outcome else {
            panic!("free token counting was charged against inference quota");
        };
        let applied = admission.snapshot.unwrap().decision["applied"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0]["limit_kind"], "requests_per_minute");
    }

    #[sqlx::test]
    async fn independent_engines_share_atomic_request_and_token_quotas(pool: PgPool) {
        for (limit, capacity, column) in [
            ("requests_per_minute", 1, "reserved_requests"),
            ("tokens_per_hour", 20, "reserved_tokens"),
        ] {
            let tenant_id = Uuid::new_v4();
            insert_global_quota(
                &pool,
                tenant_id,
                json!({"mode": "hard", "limits": {(limit): capacity}}),
            )
            .await;
            let model = Uuid::new_v4();
            let provider = Uuid::new_v4();
            let at = Utc::now();
            let admit = |pool: PgPool| async move {
                QuotaEngine::new(pool)
                    .admit(
                        Uuid::new_v4(),
                        &principal(tenant_id),
                        vec![selection(tenant_id, "direct-model", model, provider)],
                        QuotaEstimate {
                            input_upper_bound: 10,
                            requested_output_tokens: Some(10),
                            output_multiplier: 1,
                            billable: true,
                            unbounded_input: false,
                            requires_extended_pricing: false,
                            unsupported_cost_features: false,
                        },
                        at,
                    )
                    .await
                    .unwrap()
            };
            let (first, second) = tokio::join!(admit(pool.clone()), admit(pool.clone()));
            let outcomes = [first, second];
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|result| matches!(result, QuotaOutcome::Allowed(_)))
                    .count(),
                1
            );
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|result| matches!(
                        result,
                        QuotaOutcome::Rejected(_, QuotaRejection::HardLimit)
                    ))
                    .count(),
                1
            );
            let (requests, tokens, reservations): (i64, i64, i64) = sqlx::query_as(
            "SELECT reserved_requests, reserved_tokens, (SELECT count(*) FROM quota_reservations WHERE tenant_id = $1) FROM quota_windows WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
            assert_eq!(reservations, 1);
            let reserved = if column == "reserved_requests" {
                requests
            } else {
                tokens
            };
            assert_eq!(reserved, capacity);
        }
    }

    #[sqlx::test]
    async fn a_persistent_window_survives_a_new_engine_instance(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        insert_global_quota(
            &pool,
            tenant_id,
            json!({"mode": "hard", "limits": {"requests_per_minute": 1}}),
        )
        .await;
        let model = Uuid::new_v4();
        let provider = Uuid::new_v4();
        let at = Utc::now();
        let first = QuotaEngine::new(pool.clone())
            .admit(
                Uuid::new_v4(),
                &principal(tenant_id),
                vec![selection(tenant_id, "direct-model", model, provider)],
                QuotaEstimate {
                    input_upper_bound: 10,
                    requested_output_tokens: Some(10),
                    output_multiplier: 1,
                    billable: true,
                    unbounded_input: false,
                    requires_extended_pricing: false,
                    unsupported_cost_features: false,
                },
                at,
            )
            .await
            .unwrap();
        assert!(matches!(first, QuotaOutcome::Allowed(_)));
        let second = QuotaEngine::new(pool.clone())
            .admit(
                Uuid::new_v4(),
                &principal(tenant_id),
                vec![selection(tenant_id, "direct-model", model, provider)],
                QuotaEstimate {
                    input_upper_bound: 10,
                    requested_output_tokens: Some(10),
                    output_multiplier: 1,
                    billable: true,
                    unbounded_input: false,
                    requires_extended_pricing: false,
                    unsupported_cost_features: false,
                },
                at,
            )
            .await
            .unwrap();
        assert!(matches!(
            second,
            QuotaOutcome::Rejected(_, QuotaRejection::HardLimit)
        ));
        let reserved: i64 =
            sqlx::query_scalar("SELECT reserved_requests FROM quota_windows WHERE tenant_id = $1")
                .bind(tenant_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reserved, 1);
    }

    #[sqlx::test]
    async fn a_soft_limit_defers_after_exhausting_the_quality_ladder(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        insert_global_quota(
            &pool,
            tenant_id,
            json!({"mode": "soft", "limits": {"requests_per_minute": 0}}),
        )
        .await;
        let provider = Uuid::new_v4();
        let outcome = QuotaEngine::new(pool)
            .admit(
                Uuid::new_v4(),
                &principal(tenant_id),
                vec![
                    selection(tenant_id, "quality:premium", Uuid::new_v4(), provider),
                    selection(tenant_id, "quality:standard", Uuid::new_v4(), provider),
                    selection(tenant_id, "quality:economy", Uuid::new_v4(), provider),
                ],
                QuotaEstimate {
                    input_upper_bound: 10,
                    requested_output_tokens: Some(10),
                    output_multiplier: 1,
                    billable: true,
                    unbounded_input: false,
                    requires_extended_pricing: false,
                    unsupported_cost_features: false,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let QuotaOutcome::Rejected(snapshot, QuotaRejection::SoftDeferred) = outcome else {
            panic!("soft quota did not defer after exhausting every candidate");
        };
        let decision = &snapshot.decision;
        assert_eq!(decision["denial_reason"], "soft_limit_deferred");
        assert_eq!(
            decision["prior_soft_violations"].as_array().unwrap().len(),
            2
        );
    }

    #[sqlx::test]
    async fn quota_aware_routing_switch_is_independent(pool: PgPool) {
        let guarded_tenant = Uuid::new_v4();
        let routed_tenant = Uuid::new_v4();
        let guarded_model = Uuid::new_v4();
        let routed_model = Uuid::new_v4();
        let guarded_provider = Uuid::new_v4();
        let routed_provider = Uuid::new_v4();
        sqlx::query(
            r#"
            WITH inserted_providers AS (
                INSERT INTO providers (
                    id, tenant_id, provider_key, display_name, provider_type,
                    base_url, credential_ref, supported_operations, lifecycle_state
                ) VALUES
                    ($5, $1, 'guarded-provider', 'Guarded', 'openai',
                     'https://guarded.invalid', 'env://GUARDED',
                     ARRAY['openai_responses'], 'active'),
                    ($6, $2, 'routed-provider', 'Routed', 'openai',
                     'https://routed.invalid', 'env://ROUTED',
                     ARRAY['openai_responses'], 'active')
                RETURNING id
            ), inserted_models AS (
                INSERT INTO models (
                    id, tenant_id, provider_id, public_model_id, provider_model_id,
                    display_name, supported_operations, lifecycle_state
                ) VALUES
                    ($3, $1, $5, 'guarded', 'guarded', 'Guarded',
                     ARRAY['openai_responses'], 'active'),
                    ($4, $2, $6, 'guarded', 'guarded', 'Guarded',
                     ARRAY['openai_responses'], 'active')
                RETURNING id
            )
            INSERT INTO quotas (
                id, tenant_id, quota_key, revision, model_id, configuration, lifecycle_state
            ) VALUES
                (gen_random_uuid(), $1, 'model-soft', 1, $3,
                 '{"mode":"soft","limits":{"requests_per_minute":0}}', 'active'),
                (gen_random_uuid(), $2, 'model-soft', 1, $4,
                 '{"mode":"soft","limits":{"requests_per_minute":0}}', 'active')
            "#,
        )
        .bind(guarded_tenant)
        .bind(routed_tenant)
        .bind(guarded_model)
        .bind(routed_model)
        .bind(guarded_provider)
        .bind(routed_provider)
        .execute(&pool)
        .await
        .unwrap();
        let estimate = QuotaEstimate {
            input_upper_bound: 10,
            requested_output_tokens: Some(10),
            output_multiplier: 1,
            billable: true,
            unbounded_input: false,
            requires_extended_pricing: false,
            unsupported_cost_features: false,
        };
        let guarded = QuotaEngine::new(pool.clone())
            .admit_with_soft_routing(
                Uuid::new_v4(),
                &principal(guarded_tenant),
                vec![
                    selection(guarded_tenant, "guarded", guarded_model, guarded_provider),
                    selection(
                        guarded_tenant,
                        "alternate",
                        Uuid::new_v4(),
                        guarded_provider,
                    ),
                ],
                estimate,
                Utc::now(),
                false,
            )
            .await
            .unwrap();
        assert!(matches!(
            guarded,
            QuotaOutcome::Rejected(_, QuotaRejection::SoftDeferred)
        ));
        let routed = QuotaEngine::new(pool)
            .admit_with_soft_routing(
                Uuid::new_v4(),
                &principal(routed_tenant),
                vec![
                    selection(routed_tenant, "guarded", routed_model, routed_provider),
                    selection(routed_tenant, "alternate", Uuid::new_v4(), routed_provider),
                ],
                estimate,
                Utc::now(),
                true,
            )
            .await
            .unwrap();
        let QuotaOutcome::Allowed(routed) = routed else {
            panic!("quota-aware routing did not choose the unguarded candidate");
        };
        assert_eq!(routed.selection.public_model_id, "alternate");
    }

    #[sqlx::test]
    async fn only_one_gateway_process_can_own_the_concurrency_counter(pool: PgPool) {
        let first = QuotaEngine::new(pool.clone());
        let second = QuotaEngine::new(pool);
        first.acquire_single_instance_guard().await.unwrap();
        assert!(matches!(
            second.acquire_single_instance_guard().await,
            Err(QuotaError::MultipleInstances)
        ));
    }

    #[sqlx::test]
    async fn lost_single_instance_connection_permits_another_owner(pool: PgPool) {
        let first = QuotaEngine::new(pool.clone());
        let second = QuotaEngine::new(pool.clone());
        first.acquire_single_instance_guard().await.unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(1100),
                first.wait_for_single_instance_guard_loss()
            )
            .await
            .is_err()
        );
        let backend: i32 = {
            let mut guard = first.singleton_guard.lock().await;
            sqlx::query_file_scalar!("tests/current_backend_pid.sql")
                .fetch_one(&mut **guard.as_mut().unwrap())
                .await
                .unwrap()
        };
        let terminated: bool =
            sqlx::query_file_scalar!("tests/terminate_lock_connection.sql", backend)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(terminated);
        second.acquire_single_instance_guard().await.unwrap();
        assert!(first.verify_single_instance_guard().await.is_err());
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            first.wait_for_single_instance_guard_loss(),
        )
        .await
        .unwrap();
    }

    #[test]
    fn cached_usage_requires_complete_rates_and_counts_every_token_class() {
        let usage = TokenUsage {
            input_tokens: 1,
            output_tokens: 2,
            cached_input_tokens: 3,
            cache_write_input_tokens: 4,
            reasoning_tokens: 5,
        };
        assert!(
            cost_for_usage(
                &json!({
                    "schema": "token_v1",
                    "currency": "USD",
                    "input_microusd_per_million_tokens": 1_000_000,
                    "output_microusd_per_million_tokens": 1_000_000
                }),
                &usage
            )
            .is_err()
        );
        let cost = cost_for_usage(
            &json!({
                "schema": "token_v1",
                "currency": "USD",
                "input_microusd_per_million_tokens": 1_000_000,
                "output_microusd_per_million_tokens": 2_000_000,
                "cache_read_input_microusd_per_million_tokens": 3_000_000,
                "cache_write_input_microusd_per_million_tokens": 4_000_000
            }),
            &usage,
        )
        .unwrap();
        assert_eq!(cost, 30);
        assert_eq!(total_tokens(&usage).unwrap(), 15);
    }

    #[sqlx::test]
    async fn concurrency_is_returned_as_a_scheduler_constraint_without_early_occupancy(
        pool: PgPool,
    ) {
        let tenant_id = Uuid::new_v4();
        insert_global_quota(
            &pool,
            tenant_id,
            json!({"mode": "hard", "limits": {"concurrent_requests": 1}}),
        )
        .await;
        let engine = QuotaEngine::new(pool);
        let model = Uuid::new_v4();
        let provider = Uuid::new_v4();
        let first = engine
            .admit(
                Uuid::new_v4(),
                &principal(tenant_id),
                vec![selection(tenant_id, "direct-model", model, provider)],
                QuotaEstimate {
                    input_upper_bound: 10,
                    requested_output_tokens: Some(10),
                    output_multiplier: 1,
                    billable: true,
                    unbounded_input: false,
                    requires_extended_pricing: false,
                    unsupported_cost_features: false,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let QuotaOutcome::Allowed(first) = first else {
            panic!("first concurrent request was rejected");
        };
        assert_eq!(first.concurrency.len(), 1);
        assert_eq!(first.concurrency[0].limit, 1);
        let second = engine
            .admit(
                Uuid::new_v4(),
                &principal(tenant_id),
                vec![selection(tenant_id, "direct-model", model, provider)],
                QuotaEstimate {
                    input_upper_bound: 10,
                    requested_output_tokens: Some(10),
                    output_multiplier: 1,
                    billable: true,
                    unbounded_input: false,
                    requires_extended_pricing: false,
                    unsupported_cost_features: false,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let QuotaOutcome::Allowed(second) = second else {
            panic!("quota admission occupied concurrency before scheduling");
        };
        assert_eq!(second.concurrency, first.concurrency);
    }

    #[sqlx::test]
    async fn provider_usage_reconciles_once_when_the_ledger_replays(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        insert_global_quota(
            &pool,
            tenant_id,
            json!({"mode": "hard", "limits": {"tokens_per_hour": 100}}),
        )
        .await;
        let gateway_request_id = Uuid::new_v4();
        let outcome = QuotaEngine::new(pool.clone())
            .admit(
                gateway_request_id,
                &principal(tenant_id),
                vec![selection(
                    tenant_id,
                    "direct-model",
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                )],
                QuotaEstimate {
                    input_upper_bound: 10,
                    requested_output_tokens: Some(10),
                    output_multiplier: 1,
                    billable: true,
                    unbounded_input: false,
                    requires_extended_pricing: false,
                    unsupported_cost_features: false,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let QuotaOutcome::Allowed(admission) = outcome else {
            panic!("quota reservation was rejected");
        };
        let snapshot = admission.snapshot.unwrap();
        let reservation_id = snapshot.reservation_id.unwrap();
        let completed_at = Utc::now();
        let payload = json!([{
            "request": {
                "id": gateway_request_id,
                "tenant_id": tenant_id,
                "request_id": "quota-replay",
                "status": "completed",
                "created_at": completed_at,
                "started_at": completed_at,
                "completed_at": completed_at,
                "quota_reservation_id": reservation_id,
                "quota_decision": snapshot.decision,
                "quota_actual_tokens": 3,
                "quota_actual_cost_microusd": 0
            },
            "attempts": [],
            "usage_records": []
        }]);
        let before = sqlx::query_file_scalar!(
            "queries/test_quota_replay_evidence.sql",
            tenant_id,
            reservation_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before["events"], 0);
        assert_eq!(before["window_tokens"], 20);
        let mut isolated = pool.begin().await.unwrap();
        sqlx::query_file!("queries/test_set_repeatable_read.sql")
            .execute(&mut *isolated)
            .await
            .unwrap();
        let isolation_error = sqlx::query_file!("queries/test_ingest_usage_replay.sql", payload)
            .execute(&mut *isolated)
            .await
            .expect_err("unsupported isolation must be rejected");
        assert_eq!(
            isolation_error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("GW410")
        );
        isolated.rollback().await.unwrap();
        let mut omitted_reservation = payload.clone();
        omitted_reservation[0]["request"]["quota_reservation_id"] = Value::Null;
        omitted_reservation[0]["request"]["quota_actual_tokens"] = Value::Null;
        omitted_reservation[0]["request"]["quota_actual_cost_microusd"] = Value::Null;
        sqlx::query_file!("queries/test_ingest_usage_replay.sql", omitted_reservation)
            .execute(&pool)
            .await
            .expect_err("a request with a reservation must identify it");
        sqlx::query_file!("queries/test_ingest_usage_replay.sql", payload)
            .execute(&pool)
            .await
            .unwrap();
        let first = sqlx::query_file_scalar!(
            "queries/test_quota_replay_evidence.sql",
            tenant_id,
            reservation_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(first["events"], 1);
        assert_eq!(first["window_tokens"], 3);
        for fields in [
            vec!["quota_actual_tokens"],
            vec!["quota_actual_cost_microusd"],
            vec!["quota_actual_tokens", "quota_actual_cost_microusd"],
        ] {
            let mut omitted = payload.clone();
            for field in fields {
                omitted[0]["request"][field] = Value::Null;
            }
            let error = sqlx::query_file!("queries/test_ingest_usage_replay.sql", omitted)
                .execute(&pool)
                .await
                .expect_err("settled values must not be omitted in a replay");
            assert_eq!(
                error.as_database_error().unwrap().code().as_deref(),
                Some("GW409")
            );
            let unchanged = sqlx::query_file_scalar!(
                "queries/test_quota_replay_evidence.sql",
                tenant_id,
                reservation_id
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(unchanged, first);
        }
        sqlx::query_file!("queries/test_ingest_usage_replay.sql", payload)
            .execute(&pool)
            .await
            .unwrap();
        let evidence = sqlx::query_file_scalar!(
            "queries/test_quota_replay_evidence.sql",
            tenant_id,
            reservation_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(evidence, first);
        assert_eq!(evidence["events"], 1, "{evidence}");
        assert_eq!(evidence["state"], "reconciled");
        assert_eq!(evidence["actual_tokens"], 3);
        assert_eq!(evidence["window_tokens"], 3);
        let mut foreign_request = payload.clone();
        foreign_request[0]["request"]["id"] = json!(Uuid::new_v4());
        foreign_request[0]["request"]["request_id"] = json!("foreign-quota-reservation");
        sqlx::query_file!("queries/test_ingest_usage_replay.sql", foreign_request)
            .execute(&pool)
            .await
            .expect_err("a request must not use another request's reservation");
        let mut conflicting = payload.clone();
        conflicting[0]["request"]["quota_actual_tokens"] = json!(50);
        let attempt_id = Uuid::new_v4();
        conflicting[0]["attempts"] = json!([{
            "id": attempt_id, "tenant_id": tenant_id, "gateway_request_id": gateway_request_id,
            "provider": "test", "model": "test", "attempt_number": 1,
            "started_at": completed_at, "completed_at": completed_at,
            "status": "completed", "estimated_usage": {}, "actual_usage": {}
        }]);
        conflicting[0]["usage_records"] = json!([{
            "id": Uuid::new_v4(), "tenant_id": tenant_id, "gateway_request_id": gateway_request_id,
            "provider_attempt_id": attempt_id, "source": "provider_reported",
            "input_tokens": 50, "output_tokens": 0, "cached_input_tokens": 0,
            "cache_write_input_tokens": 0, "reasoning_tokens": 0, "recorded_at": completed_at
        }]);
        let conflict = sqlx::query_file!("queries/test_ingest_usage_replay.sql", conflicting)
            .execute(&pool)
            .await
            .expect_err("conflicting usage must be rejected");
        assert_eq!(
            conflict
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("GW409")
        );
        conflicting[0]["request"]["quota_actual_tokens"] = json!(3);
        let extra_usage = sqlx::query_file!("queries/test_ingest_usage_replay.sql", conflicting)
            .execute(&pool)
            .await
            .expect_err("new usage on an existing request must be rejected");
        assert_eq!(
            extra_usage
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("GW409")
        );
        let after_conflict = sqlx::query_file_scalar!(
            "queries/test_quota_replay_evidence.sql",
            tenant_id,
            reservation_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(after_conflict, first);
        let usage = sqlx::query_file_scalar!(
            "queries/test_usage_total_for_request.sql",
            gateway_request_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(usage, 0);
        let batch_id = Uuid::new_v4();
        let mut first_in_batch = payload[0].clone();
        first_in_batch["request"]["id"] = json!(batch_id);
        first_in_batch["request"]["request_id"] = json!("conflicting-batch");
        first_in_batch["request"]["quota_reservation_id"] = Value::Null;
        first_in_batch["request"]["quota_actual_tokens"] = Value::Null;
        first_in_batch["request"]["quota_actual_cost_microusd"] = Value::Null;
        let mut second_in_batch = first_in_batch.clone();
        second_in_batch["attempts"] = conflicting[0]["attempts"].clone();
        second_in_batch["attempts"][0]["gateway_request_id"] = json!(batch_id);
        second_in_batch["usage_records"] = conflicting[0]["usage_records"].clone();
        second_in_batch["usage_records"][0]["gateway_request_id"] = json!(batch_id);
        let batch = json!([first_in_batch, second_in_batch]);
        sqlx::query_file!("queries/test_ingest_usage_replay.sql", batch)
            .execute(&pool)
            .await
            .expect_err("conflicting requests in one batch must be rejected");
        let batch_counts = sqlx::query_file!("queries/test_usage_replay_counts.sql", batch_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            (
                batch_counts.requests,
                batch_counts.attempts,
                batch_counts.usage
            ),
            (0, 0, 0)
        );
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query("UPDATE quota_windows SET reserved_tokens = 99 WHERE tenant_id = $1")
            .bind(tenant_id)
            .execute(&mut *transaction)
            .await
            .unwrap();
        let rebuilt: i64 = sqlx::query_scalar("SELECT recompute_quota_windows($1)")
            .bind(tenant_id)
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
        let recomputed: i64 = sqlx::query_scalar(
            "SELECT reserved_tokens FROM quota_windows WHERE tenant_id = $1 AND limit_kind = 'tokens_per_hour'",
        )
        .bind(tenant_id)
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        assert_eq!(rebuilt, 1);
        assert_eq!(recomputed, 3);
        transaction.commit().await.unwrap();
    }

    #[sqlx::test]
    async fn expired_quota_reservation_accepts_late_usage_once(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        insert_global_quota(
            &pool,
            tenant_id,
            json!({"mode": "hard", "limits": {"tokens_per_hour": 100}}),
        )
        .await;
        let request_id = Uuid::new_v4();
        let admitted = QuotaEngine::new(pool.clone())
            .admit(
                request_id,
                &principal(tenant_id),
                vec![selection(
                    tenant_id,
                    "direct-model",
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                )],
                QuotaEstimate {
                    input_upper_bound: 10,
                    requested_output_tokens: Some(10),
                    output_multiplier: 1,
                    billable: true,
                    unbounded_input: false,
                    requires_extended_pricing: false,
                    unsupported_cost_features: false,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let QuotaOutcome::Allowed(admission) = admitted else {
            panic!("quota reservation rejected")
        };
        let snapshot = admission.snapshot.unwrap();
        let reservation_id = snapshot.reservation_id.unwrap();
        let before = sqlx::query_file_scalar!(
            "queries/test_quota_replay_evidence.sql",
            tenant_id,
            reservation_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (before["state"].as_str(), before["window_tokens"].as_i64()),
            (Some("active"), Some(20))
        );
        let expired = sqlx::query_file!(
            "queries/test_expire_quota_reservation.sql",
            tenant_id,
            reservation_id
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(expired.rows_affected(), 1);
        let completed_at = Utc::now();
        let payload = json!([{
            "request": {
                "id": request_id, "tenant_id": tenant_id, "request_id": "late-quota-usage",
                "status": "completed", "created_at": completed_at, "started_at": completed_at,
                "completed_at": completed_at, "quota_reservation_id": reservation_id,
                "quota_decision": snapshot.decision, "quota_actual_tokens": 7,
                "quota_actual_cost_microusd": 0
            },
            "attempts": [], "usage_records": []
        }]);
        for _ in 0..2 {
            sqlx::query_file!("queries/test_ingest_usage_replay.sql", payload)
                .execute(&pool)
                .await
                .unwrap();
        }
        let after = sqlx::query_file_scalar!(
            "queries/test_quota_replay_evidence.sql",
            tenant_id,
            reservation_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (
                after["state"].as_str(),
                after["window_tokens"].as_i64(),
                after["events"].as_i64()
            ),
            (Some("reconciled"), Some(7), Some(1))
        );
    }

    #[sqlx::test]
    async fn late_usage_updates_original_window_after_real_expiration(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        insert_global_quota(
            &pool,
            tenant_id,
            json!({"mode": "hard", "limits": {"tokens_per_hour": 100}}),
        )
        .await;
        let engine = QuotaEngine::new(pool.clone());
        let request_id = Uuid::new_v4();
        let at = Utc::now();
        let later = at + Duration::hours(25);
        let estimate = || QuotaEstimate {
            input_upper_bound: 10,
            requested_output_tokens: Some(10),
            output_multiplier: 1,
            billable: true,
            unbounded_input: false,
            requires_extended_pricing: false,
            unsupported_cost_features: false,
        };
        let candidates = || {
            vec![selection(
                tenant_id,
                "direct-model",
                Uuid::new_v4(),
                Uuid::new_v4(),
            )]
        };
        let first = engine
            .admit(
                request_id,
                &principal(tenant_id),
                candidates(),
                estimate(),
                at,
            )
            .await
            .unwrap();
        let QuotaOutcome::Allowed(first) = first else {
            panic!("first reservation rejected")
        };
        let snapshot = first.snapshot.unwrap();
        let reservation_id = snapshot.reservation_id.unwrap();
        let second = engine
            .admit(
                Uuid::new_v4(),
                &principal(tenant_id),
                candidates(),
                estimate(),
                later,
            )
            .await
            .unwrap();
        assert!(matches!(second, QuotaOutcome::Allowed(_)));
        let before = sqlx::query_file_scalar!(
            "queries/test_quota_expiration_windows.sql",
            tenant_id,
            reservation_id,
            at,
            later
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (
                before["state"].as_str(),
                before["events"].as_i64(),
                before["original"].as_i64(),
                before["later"].as_i64()
            ),
            (Some("expired"), Some(0), Some(20), Some(20))
        );
        let payload = json!([{
            "request": {
                "id": request_id, "tenant_id": tenant_id, "request_id": "late-cross-window",
                "status": "completed", "created_at": at, "started_at": at,
                "completed_at": later, "quota_reservation_id": reservation_id,
                "quota_decision": snapshot.decision, "quota_actual_tokens": 7,
                "quota_actual_cost_microusd": 0
            }, "attempts": [], "usage_records": []
        }]);
        for _ in 0..2 {
            sqlx::query_file!("queries/test_ingest_usage_replay.sql", payload)
                .execute(&pool)
                .await
                .unwrap();
        }
        let after = sqlx::query_file_scalar!(
            "queries/test_quota_expiration_windows.sql",
            tenant_id,
            reservation_id,
            at,
            later
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (
                after["state"].as_str(),
                after["events"].as_i64(),
                after["original"].as_i64(),
                after["later"].as_i64()
            ),
            (Some("reconciled"), Some(1), Some(7), Some(20))
        );
    }

    #[sqlx::test]
    async fn usage_ingest_waits_for_the_same_tenant_lock_as_admission(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let at = Utc::now();
        let payload = json!([{
            "request": {
                "id": request_id,
                "tenant_id": tenant_id,
                "request_id": "lock-barrier",
                "status": "completed",
                "created_at": at,
                "started_at": at,
                "completed_at": at
            },
            "attempts": [],
            "usage_records": []
        }]);
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, $2))")
            .bind(tenant_id)
            .bind(ADMISSION_LOCK_NAMESPACE)
            .execute(&mut *blocker)
            .await
            .unwrap();
        let ingest_pool = pool.clone();
        let mut ingest = tokio::spawn(async move {
            sqlx::query("SELECT ingest_usage_envelopes($1)")
                .bind(payload)
                .execute(&ingest_pool)
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut ingest)
                .await
                .is_err(),
            "usage reconciliation bypassed the admission tenant lock"
        );
        blocker.rollback().await.unwrap();
        ingest.await.unwrap().unwrap();
    }

    #[sqlx::test]
    async fn all_seven_scopes_apply_in_one_admission(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();
        let client_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let credential_id = Uuid::new_v4();
        let provider_id = Uuid::new_v4();
        let model_id = Uuid::new_v4();
        let quota_ids = (0..7).map(|_| Uuid::new_v4()).collect::<Vec<_>>();
        sqlx::query(
            r#"
            WITH inserted_user AS (
                INSERT INTO users (id, tenant_id, display_name)
                VALUES ($1, $2, 'user') RETURNING id
            ), inserted_device AS (
                INSERT INTO devices (id, tenant_id, display_name)
                VALUES ($3, $2, 'device') RETURNING id
            ), inserted_client AS (
                INSERT INTO clients (id, tenant_id, display_name)
                VALUES ($4, $2, 'client') RETURNING id
            ), inserted_agent AS (
                INSERT INTO agents (id, tenant_id, display_name)
                VALUES ($5, $2, 'agent') RETURNING id
            ), inserted_credential AS (
                INSERT INTO credentials (
                    id, tenant_id, type, display_name, secret_hash, key_prefix,
                    fingerprint, hash_key_id, user_id, device_id, client_id, agent_id
                ) SELECT $6, $2, 'client_api_key', 'credential', $7, 'prefix', $8, 'v1',
                         u.id, d.id, c.id, a.id
                  FROM inserted_user u, inserted_device d, inserted_client c, inserted_agent a
                RETURNING id
            ), inserted_provider AS (
                INSERT INTO providers (
                    id, tenant_id, provider_key, display_name, provider_type,
                    base_url, credential_ref, supported_operations, lifecycle_state
                ) VALUES (
                    $9, $2, 'scope-provider', 'provider', 'openai',
                    'https://scope.invalid', 'env://SCOPE_KEY',
                    ARRAY['openai_responses'], 'active'
                ) RETURNING id
            ), inserted_model AS (
                INSERT INTO models (
                    id, tenant_id, provider_id, public_model_id, provider_model_id,
                    display_name, supported_operations, lifecycle_state
                ) SELECT $10, $2, id, 'scope-model', 'scope-model', 'model',
                         ARRAY['openai_responses'], 'active'
                  FROM inserted_provider RETURNING id
            ), inserted_quotas AS (
            INSERT INTO quotas (
                id, tenant_id, quota_key, revision, user_id, device_id, client_id,
                agent_id, credential_id, model_id, provider_id, configuration,
                lifecycle_state
            ) VALUES
                ($11, $2, 'user', 1, $1, NULL, NULL, NULL, NULL, NULL, NULL, $18, 'active'),
                ($12, $2, 'device', 1, NULL, $3, NULL, NULL, NULL, NULL, NULL, $18, 'active'),
                ($13, $2, 'client', 1, NULL, NULL, $4, NULL, NULL, NULL, NULL, $18, 'active'),
                ($14, $2, 'agent', 1, NULL, NULL, NULL, $5, NULL, NULL, NULL, $18, 'active'),
                ($15, $2, 'credential', 1, NULL, NULL, NULL, NULL, $6, NULL, NULL, $18, 'active'),
                ($16, $2, 'model', 1, NULL, NULL, NULL, NULL, NULL, $10, NULL, $18, 'active'),
                ($17, $2, 'provider', 1, NULL, NULL, NULL, NULL, NULL, NULL, $9, $18, 'active')
            RETURNING id
            )
            UPDATE quotas SET quota_key = rpad(quota_key, 200, 'x')
            WHERE tenant_id = $2 AND id IN (SELECT id FROM inserted_quotas)
            "#,
        )
        .bind(user_id)
        .bind(tenant_id)
        .bind(device_id)
        .bind(client_id)
        .bind(agent_id)
        .bind(credential_id)
        .bind([1_u8; 32].as_slice())
        .bind([2_u8; 32].as_slice())
        .bind(provider_id)
        .bind(model_id)
        .bind(quota_ids[0])
        .bind(quota_ids[1])
        .bind(quota_ids[2])
        .bind(quota_ids[3])
        .bind(quota_ids[4])
        .bind(quota_ids[5])
        .bind(quota_ids[6])
        .bind(json!({"mode": "hard", "limits": {
            "requests_per_minute": 100,
            "estimated_tokens_per_minute": 100_000,
            "tokens_per_hour": 100_000,
            "tokens_per_day": 100_000,
            "concurrent_requests": 100,
            "cost_per_day_microusd": 100_000,
            "cost_per_month_microusd": 100_000
        }}))
        .execute(&pool)
        .await
        .unwrap();
        let outcome = QuotaEngine::new(pool.clone())
            .admit(
                Uuid::new_v4(),
                &PrincipalContext {
                    tenant_id,
                    credential_id,
                    credential_name: "credential".to_owned(),
                    user_id: Some(user_id),
                    user_name: Some("user".to_owned()),
                    device_id: Some(device_id),
                    device_name: Some("device".to_owned()),
                    client_id: Some(client_id),
                    client_name: Some("client".to_owned()),
                    agent_id: Some(agent_id),
                    agent_name: Some("agent".to_owned()),
                },
                vec![selection(tenant_id, "scope-model", model_id, provider_id)],
                QuotaEstimate {
                    input_upper_bound: 10,
                    requested_output_tokens: Some(10),
                    output_multiplier: 1,
                    billable: true,
                    unbounded_input: false,
                    requires_extended_pricing: false,
                    unsupported_cost_features: false,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let QuotaOutcome::Allowed(admission) = outcome else {
            panic!("matching seven-scope request was rejected");
        };
        let decision = admission.snapshot.unwrap().decision;
        assert_eq!(decision["applied"].as_array().unwrap().len(), 42);
        assert!(serde_json::to_vec(&decision).unwrap().len() <= 65_536);
        let windows: i64 =
            sqlx::query_scalar("SELECT count(*) FROM quota_windows WHERE tenant_id = $1")
                .bind(tenant_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(windows, 42);
    }
}
