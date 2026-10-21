use std::{
    cmp::Ordering,
    collections::HashMap,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};

use crate::{error::GatewayError, model_registry::ModelSelection};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoutingConfig {
    pub health_aware: bool,
    pub load_aware: bool,
    pub cost_aware: bool,
    pub quota_aware: bool,
    pub auto_quality: bool,
    pub failure_cooldown: chrono::Duration,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            health_aware: true,
            load_aware: true,
            cost_aware: false,
            quota_aware: true,
            auto_quality: true,
            failure_cooldown: chrono::Duration::seconds(30),
        }
    }
}

#[derive(Clone)]
pub struct RoutingEngine {
    config: RoutingConfig,
    state: Arc<Mutex<HashMap<String, ProviderState>>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct ProviderState {
    active: usize,
    next_generation: u64,
    latest_applied_generation: u64,
    consecutive_transient_failures: u32,
    latency_ewma_ms: u64,
    last_completed_at: Option<DateTime<Utc>>,
    failure_until: Option<DateTime<Utc>>,
    half_open_claimed: bool,
}

pub struct RankedCandidates {
    pub candidates: Vec<ModelSelection>,
    preferred_provider: String,
    preferred_model: String,
    preferred_reason: &'static str,
    half_open_claim: Option<HalfOpenClaim>,
}

impl RankedCandidates {
    pub fn reason_for(&self, selection: &ModelSelection, quota_aware: bool) -> &'static str {
        if selection.provider_key == self.preferred_provider
            && selection.provider_model_id == self.preferred_model
        {
            self.preferred_reason
        } else if quota_aware {
            "quota"
        } else {
            "registry"
        }
    }

    pub fn take_claim_for(&mut self, selection: &ModelSelection) -> Option<HalfOpenClaim> {
        self.half_open_claim
            .take_if(|claim| claim.provider == selection.provider_key)
    }
}

pub struct HalfOpenClaim {
    engine: RoutingEngine,
    provider: String,
    active: bool,
}

pub struct RoutingLease {
    engine: RoutingEngine,
    provider: String,
    started_at: DateTime<Utc>,
    generation: u64,
    half_open: bool,
    active: bool,
}

impl RoutingEngine {
    pub fn new(config: RoutingConfig, providers: impl IntoIterator<Item = String>) -> Self {
        let state = providers
            .into_iter()
            .map(|provider| (provider, ProviderState::default()))
            .collect();
        Self {
            config,
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn quota_aware(&self) -> bool {
        self.config.quota_aware
    }

    pub fn rank(&self, candidates: Vec<ModelSelection>) -> Result<RankedCandidates, GatewayError> {
        self.rank_at(candidates, Utc::now())
    }

    fn rank_at(
        &self,
        candidates: Vec<ModelSelection>,
        now: DateTime<Utc>,
    ) -> Result<RankedCandidates, GatewayError> {
        let original = candidates
            .first()
            .cloned()
            .ok_or(GatewayError::ModelForbidden)?;
        let original_provider = original.provider_key.clone();
        let original_model = original.provider_model_id.clone();
        let original_quality = original.selector.quality().map(ToOwned::to_owned);
        let mut state = self
            .state
            .lock()
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        let mut indexed = candidates.into_iter().enumerate().collect::<Vec<_>>();
        if self.config.health_aware {
            indexed.retain(|(_, candidate)| {
                state
                    .get(&candidate.provider_key)
                    .copied()
                    .is_some_and(|provider| provider_is_eligible(provider, now))
            });
        }
        if indexed.is_empty() {
            return Err(GatewayError::ProviderUnavailable);
        }
        indexed.sort_by(|(left_index, left), (right_index, right)| {
            let left_score = score(
                self.config,
                &state,
                left,
                original_quality.as_deref(),
                *left_index,
                now,
            );
            let right_score = score(
                self.config,
                &state,
                right,
                original_quality.as_deref(),
                *right_index,
                now,
            );
            left_score.cmp(&right_score)
        });
        let candidates = indexed
            .into_iter()
            .map(|(_, candidate)| candidate)
            .collect::<Vec<_>>();
        let preferred = candidates.first().ok_or(GatewayError::ModelForbidden)?;
        let preferred_reason = if preferred.provider_key == original_provider
            && preferred.provider_model_id == original_model
        {
            "registry"
        } else {
            changed_reason(self.config, &state, &original, preferred, now)
        };
        let half_open_claim = if self.config.health_aware
            && state
                .get(&preferred.provider_key)
                .copied()
                .is_some_and(|provider| provider_is_half_open(provider, now))
        {
            let provider = state
                .get_mut(&preferred.provider_key)
                .ok_or(GatewayError::InvalidConfiguration)?;
            provider.half_open_claimed = true;
            Some(HalfOpenClaim {
                engine: self.clone(),
                provider: preferred.provider_key.clone(),
                active: true,
            })
        } else {
            None
        };
        Ok(RankedCandidates {
            preferred_provider: preferred.provider_key.clone(),
            preferred_model: preferred.provider_model_id.clone(),
            preferred_reason,
            half_open_claim,
            candidates,
        })
    }

    pub fn begin(
        &self,
        provider: &str,
        started_at: DateTime<Utc>,
        mut half_open_claim: Option<HalfOpenClaim>,
    ) -> Result<RoutingLease, GatewayError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        let provider_state = state
            .get_mut(provider)
            .ok_or(GatewayError::InvalidConfiguration)?;
        let half_open = if let Some(claim) = half_open_claim.as_mut() {
            if !claim.active || claim.provider != provider {
                return Err(GatewayError::InvalidConfiguration);
            }
            claim.active = false;
            true
        } else {
            false
        };
        if provider_state.consecutive_transient_failures > 0 && !half_open {
            return Err(GatewayError::ProviderUnavailable);
        }
        provider_state.active = provider_state
            .active
            .checked_add(1)
            .ok_or(GatewayError::InvalidConfiguration)?;
        provider_state.next_generation = provider_state.next_generation.saturating_add(1);
        Ok(RoutingLease {
            engine: self.clone(),
            provider: provider.to_owned(),
            started_at,
            generation: provider_state.next_generation,
            half_open,
            active: true,
        })
    }
}

impl RoutingLease {
    pub fn conclude(
        &mut self,
        completed_at: DateTime<Utc>,
        success: bool,
        error_type: Option<&str>,
    ) {
        let Ok(mut state) = self.engine.state.lock() else {
            return;
        };
        let Some(provider) = state.get_mut(&self.provider) else {
            return;
        };
        release_active(provider, &mut self.active);
        if self.half_open {
            provider.half_open_claimed = false;
        }
        if self.generation < provider.latest_applied_generation {
            return;
        }
        provider.latest_applied_generation = self.generation;
        provider.last_completed_at = Some(completed_at);
        let latency_ms = (completed_at - self.started_at).num_milliseconds().max(0) as u64;
        provider.latency_ewma_ms = if provider.latency_ewma_ms == 0 {
            latency_ms
        } else {
            provider
                .latency_ewma_ms
                .saturating_mul(3)
                .saturating_add(latency_ms)
                / 4
        };
        if success || (self.half_open && !error_type.is_some_and(is_transient)) {
            provider.consecutive_transient_failures = 0;
            provider.failure_until = None;
        } else if error_type.is_some_and(is_transient) {
            provider.consecutive_transient_failures =
                provider.consecutive_transient_failures.saturating_add(1);
            provider.failure_until =
                completed_at.checked_add_signed(self.engine.config.failure_cooldown);
        }
    }
}

impl Drop for RoutingLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Ok(mut state) = self.engine.state.lock()
            && let Some(provider) = state.get_mut(&self.provider)
        {
            release_active(provider, &mut self.active);
            if self.half_open {
                provider.half_open_claimed = false;
            }
        }
    }
}

impl Drop for HalfOpenClaim {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Ok(mut state) = self.engine.state.lock()
            && let Some(provider) = state.get_mut(&self.provider)
        {
            provider.half_open_claimed = false;
            self.active = false;
        }
    }
}

fn release_active(provider: &mut ProviderState, active: &mut bool) {
    if *active {
        provider.active = provider.active.saturating_sub(1);
        *active = false;
    }
}

#[derive(Eq, PartialEq)]
struct CandidateScore {
    quality_group: u8,
    failures: u32,
    latency_ms: u64,
    active: usize,
    cost: u128,
    original_index: usize,
}

impl Ord for CandidateScore {
    fn cmp(&self, other: &Self) -> Ordering {
        (
            self.quality_group,
            self.failures,
            self.latency_ms,
            self.active,
            self.cost,
            self.original_index,
        )
            .cmp(&(
                other.quality_group,
                other.failures,
                other.latency_ms,
                other.active,
                other.cost,
                other.original_index,
            ))
    }
}

impl PartialOrd for CandidateScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn score(
    config: RoutingConfig,
    state: &HashMap<String, ProviderState>,
    candidate: &ModelSelection,
    original_quality: Option<&str>,
    original_index: usize,
    now: DateTime<Utc>,
) -> CandidateScore {
    let provider = state
        .get(&candidate.provider_key)
        .copied()
        .unwrap_or_default();
    let (failures, latency_ms) = health_score(provider, now);
    CandidateScore {
        quality_group: u8::from(
            !config.auto_quality && candidate.selector.quality() != original_quality,
        ),
        failures: if config.health_aware { failures } else { 0 },
        latency_ms: if config.health_aware { latency_ms } else { 0 },
        active: if config.load_aware {
            provider.active
        } else {
            0
        },
        cost: if config.cost_aware {
            cost_score(&candidate.cost_metadata)
        } else {
            0
        },
        original_index,
    }
}

fn changed_reason(
    config: RoutingConfig,
    state: &HashMap<String, ProviderState>,
    original: &ModelSelection,
    selected: &ModelSelection,
    now: DateTime<Utc>,
) -> &'static str {
    let original_state = state
        .get(&original.provider_key)
        .copied()
        .unwrap_or_default();
    let selected_state = state
        .get(&selected.provider_key)
        .copied()
        .unwrap_or_default();
    let original_health = health_score(original_state, now);
    let selected_health = health_score(selected_state, now);
    if config.health_aware && selected_health < original_health {
        "health"
    } else if config.load_aware && selected_state.active < original_state.active {
        "load"
    } else if config.cost_aware
        && cost_score(&selected.cost_metadata) < cost_score(&original.cost_metadata)
    {
        "cost"
    } else if config.auto_quality {
        "quality"
    } else {
        "registry"
    }
}

fn health_score(provider: ProviderState, now: DateTime<Utc>) -> (u32, u64) {
    if provider_is_half_open(provider, now) {
        return (0, 0);
    }
    (
        provider.consecutive_transient_failures,
        provider
            .last_completed_at
            .map_or(u64::MAX, |_| provider.latency_ewma_ms),
    )
}

fn provider_is_half_open(provider: ProviderState, now: DateTime<Utc>) -> bool {
    provider.consecutive_transient_failures > 0
        && !provider.half_open_claimed
        && provider.active == 0
        && provider.failure_until.is_some_and(|until| until <= now)
}

fn provider_is_eligible(provider: ProviderState, now: DateTime<Utc>) -> bool {
    provider.consecutive_transient_failures == 0 || provider_is_half_open(provider, now)
}

fn cost_score(metadata: &serde_json::Value) -> u128 {
    const OPTIONAL_KEYS: [&str; 2] = [
        "cache_read_input_microusd_per_million_tokens",
        "cache_write_input_microusd_per_million_tokens",
    ];
    if metadata.get("schema").and_then(serde_json::Value::as_str) != Some("token_v1") {
        return u128::MAX;
    }
    let Some(input) = metadata
        .get("input_microusd_per_million_tokens")
        .and_then(serde_json::Value::as_u64)
    else {
        return u128::MAX;
    };
    let Some(output) = metadata
        .get("output_microusd_per_million_tokens")
        .and_then(serde_json::Value::as_u64)
    else {
        return u128::MAX;
    };
    OPTIONAL_KEYS.iter().fold(
        u128::from(input).saturating_add(u128::from(output)),
        |total, key| {
            total.saturating_add(u128::from(
                metadata
                    .get(key)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default(),
            ))
        },
    )
}

fn is_transient(error_type: &str) -> bool {
    matches!(
        error_type,
        "408"
            | "500"
            | "502"
            | "503"
            | "504"
            | "upstream_unavailable"
            | "upstream_body_error"
            | "provider_response_failed"
            | "provider_stream_error"
    )
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::{RoutingConfig, RoutingEngine};
    use crate::model_registry::{
        GatewayOperation, ModelSelection, ModelSelector, ProviderKind, ProviderProfile, QualityTier,
    };

    #[test]
    fn transient_failure_routes_only_the_next_request() {
        let engine = RoutingEngine::new(RoutingConfig::default(), providers());
        let candidates = vec![
            candidate("primary", QualityTier::Advanced, 4),
            candidate("secondary", QualityTier::Standard, 5),
        ];
        assert_eq!(
            engine.rank(candidates.clone()).unwrap().candidates[0].provider_key,
            "primary"
        );
        let started = Utc::now();
        let mut only_attempt = engine.begin("primary", started, None).unwrap();
        only_attempt.conclude(started + Duration::milliseconds(20), false, Some("503"));

        let next = engine.rank(candidates).unwrap();
        assert_eq!(next.candidates[0].provider_key, "secondary");
        assert_eq!(next.reason_for(&next.candidates[0], true), "health");
        let mut half_open = engine
            .rank_at(
                vec![
                    candidate("primary", QualityTier::Advanced, 4),
                    candidate("secondary", QualityTier::Standard, 5),
                ],
                started + Duration::seconds(31),
            )
            .unwrap();
        assert_eq!(half_open.candidates[0].provider_key, "primary");
        let concurrent = engine
            .rank_at(
                vec![
                    candidate("primary", QualityTier::Advanced, 4),
                    candidate("secondary", QualityTier::Standard, 5),
                ],
                started + Duration::seconds(31),
            )
            .unwrap();
        assert_eq!(concurrent.candidates[0].provider_key, "secondary");
        let claim = half_open.take_claim_for(&half_open.candidates[0].clone());
        let _probe = engine
            .begin("primary", started + Duration::seconds(31), claim)
            .unwrap();
    }

    #[test]
    fn provider_rate_limit_does_not_open_the_gateway_circuit() {
        let engine = RoutingEngine::new(RoutingConfig::default(), ["primary".to_owned()]);
        let started = Utc::now();
        let mut attempt = engine.begin("primary", started, None).unwrap();
        attempt.conclude(started + Duration::milliseconds(1), false, Some("429"));

        let ranked = engine
            .rank_at(
                vec![candidate("primary", QualityTier::Advanced, 4)],
                started + Duration::milliseconds(2),
            )
            .unwrap();
        assert_eq!(ranked.candidates[0].provider_key, "primary");
    }

    #[test]
    fn later_started_result_wins_when_requests_complete_out_of_order() {
        let engine = RoutingEngine::new(RoutingConfig::default(), providers());
        let started = Utc::now();
        let mut old = engine.begin("primary", started, None).unwrap();
        let mut newer = engine
            .begin("primary", started + Duration::milliseconds(1), None)
            .unwrap();
        newer.conclude(started + Duration::milliseconds(2), true, None);
        old.conclude(started + Duration::milliseconds(3), false, Some("503"));
        let ranked = engine
            .rank(vec![
                candidate("primary", QualityTier::Advanced, 4),
                candidate("secondary", QualityTier::Standard, 5),
            ])
            .unwrap();
        assert_eq!(ranked.candidates[0].provider_key, "primary");
    }

    #[test]
    fn half_open_allows_one_probe_and_rejects_parallel_claims() {
        let engine = RoutingEngine::new(RoutingConfig::default(), ["primary".to_owned()]);
        let started = Utc::now();
        let mut failed = engine.begin("primary", started, None).unwrap();
        failed.conclude(started + Duration::milliseconds(1), false, Some("503"));

        let probe_at = started + Duration::seconds(31);
        let mut probe = engine
            .rank_at(
                vec![candidate("primary", QualityTier::Advanced, 4)],
                probe_at,
            )
            .unwrap();
        assert!(matches!(
            engine.rank_at(
                vec![candidate("primary", QualityTier::Advanced, 4)],
                probe_at,
            ),
            Err(crate::error::GatewayError::ProviderUnavailable)
        ));
        let selected = probe.candidates[0].clone();
        let claim = probe.take_claim_for(&selected);
        let _lease = engine.begin("primary", probe_at, claim).unwrap();
        assert!(matches!(
            engine.rank_at(
                vec![candidate("primary", QualityTier::Advanced, 4)],
                probe_at,
            ),
            Err(crate::error::GatewayError::ProviderUnavailable)
        ));
    }

    #[test]
    fn explicit_stream_failure_penalizes_the_provider() {
        let engine = RoutingEngine::new(RoutingConfig::default(), providers());
        let started = Utc::now();
        let mut stream = engine.begin("primary", started, None).unwrap();
        stream.conclude(
            started + Duration::milliseconds(1),
            false,
            Some("provider_stream_error"),
        );
        let ranked = engine
            .rank_at(
                vec![
                    candidate("primary", QualityTier::Advanced, 4),
                    candidate("secondary", QualityTier::Standard, 5),
                ],
                started + Duration::milliseconds(2),
            )
            .unwrap();
        assert_eq!(ranked.candidates[0].provider_key, "secondary");
    }

    #[test]
    fn load_cost_and_quality_switches_are_independent() {
        let started = Utc::now();
        let load_engine = RoutingEngine::new(
            RoutingConfig {
                health_aware: false,
                load_aware: true,
                cost_aware: false,
                quota_aware: false,
                auto_quality: true,
                failure_cooldown: Duration::seconds(30),
            },
            providers(),
        );
        let _active = load_engine.begin("primary", started, None).unwrap();
        let candidates = vec![
            candidate("primary", QualityTier::Advanced, 1),
            candidate("secondary", QualityTier::Standard, 9),
        ];
        assert_eq!(
            load_engine.rank(candidates.clone()).unwrap().candidates[0].provider_key,
            "secondary"
        );

        let cost_engine = RoutingEngine::new(
            RoutingConfig {
                health_aware: false,
                load_aware: false,
                cost_aware: true,
                quota_aware: false,
                auto_quality: true,
                failure_cooldown: Duration::seconds(30),
            },
            providers(),
        );
        assert_eq!(
            cost_engine.rank(candidates.clone()).unwrap().candidates[0].provider_key,
            "primary"
        );
        let mut incomplete_price = candidate("primary", QualityTier::Advanced, 1);
        incomplete_price.cost_metadata = json!({
            "schema": "token_v1",
            "input_microusd_per_million_tokens": 1
        });
        assert_eq!(
            cost_engine
                .rank(vec![
                    incomplete_price,
                    candidate("secondary", QualityTier::Standard, 9),
                ])
                .unwrap()
                .candidates[0]
                .provider_key,
            "secondary"
        );

        let quality_guard = RoutingEngine::new(
            RoutingConfig {
                health_aware: true,
                load_aware: true,
                cost_aware: true,
                quota_aware: false,
                auto_quality: false,
                failure_cooldown: Duration::seconds(30),
            },
            providers(),
        );
        let _active = quality_guard.begin("primary", started, None).unwrap();
        assert_eq!(
            quality_guard.rank(candidates).unwrap().candidates[0].provider_key,
            "primary"
        );
    }

    #[test]
    fn measured_health_change_and_reason_use_the_same_score() {
        let engine = RoutingEngine::new(RoutingConfig::default(), providers());
        let started = Utc::now();
        let mut secondary = engine.begin("secondary", started, None).unwrap();
        secondary.conclude(started + Duration::milliseconds(5), true, None);
        let ranked = engine
            .rank(vec![
                candidate("primary", QualityTier::Advanced, 4),
                candidate("secondary", QualityTier::Standard, 5),
            ])
            .unwrap();
        assert_eq!(ranked.candidates[0].provider_key, "secondary");
        assert_eq!(ranked.reason_for(&ranked.candidates[0], true), "health");
    }

    fn providers() -> Vec<String> {
        vec!["primary".to_owned(), "secondary".to_owned()]
    }

    fn candidate(provider: &str, quality: QualityTier, cost: u64) -> ModelSelection {
        let operation = GatewayOperation::OpenAiResponses;
        ModelSelection {
            selector: ModelSelector::Quality(quality),
            alias_id: Some(Uuid::new_v4()),
            alias_version: Some(1),
            model_id: Uuid::new_v4(),
            model_version: 1,
            public_model_id: format!("{provider}-public"),
            provider_model_id: format!("{provider}-target"),
            provider_id: Uuid::new_v4(),
            provider_version: 1,
            provider_key: provider.to_owned(),
            description: None,
            context_window: None,
            latency_class: None,
            supports_streaming: false,
            supports_tools: false,
            supports_reasoning: false,
            supports_images: false,
            supports_structured_output: false,
            max_output_tokens: Some(1024),
            cost_metadata: json!({
                "schema": "token_v1",
                "input_microusd_per_million_tokens": cost,
                "output_microusd_per_million_tokens": cost,
            }),
            profile: ProviderProfile::bearer_for(
                ProviderKind::OpenAi,
                format!("env://{provider}"),
                provider,
                "https://example.test",
                vec![operation],
                vec![Uuid::nil()],
                "secret",
            ),
            model_supported_operations: vec![operation],
            provider_supported_operations: vec![operation],
        }
    }
}
