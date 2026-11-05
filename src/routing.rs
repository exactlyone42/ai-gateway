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
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            health_aware: true,
            load_aware: true,
            cost_aware: false,
            quota_aware: true,
            auto_quality: true,
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
    latency_ewma_ms: u64,
    last_completed_at: Option<DateTime<Utc>>,
}

pub struct RankedCandidates {
    pub candidates: Vec<ModelSelection>,
    preferred_provider: String,
    preferred_model: String,
    preferred_reason: &'static str,
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
}

pub struct RoutingLease {
    engine: RoutingEngine,
    provider: String,
    started_at: DateTime<Utc>,
    generation: u64,
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
        let original = candidates
            .first()
            .cloned()
            .ok_or(GatewayError::ModelForbidden)?;
        let original_quality = original.selector.quality().map(ToOwned::to_owned);
        let state = self
            .state
            .lock()
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        let mut indexed = candidates.into_iter().enumerate().collect::<Vec<_>>();
        indexed.sort_by(|(left_index, left), (right_index, right)| {
            let left_score = score(
                self.config,
                &state,
                left,
                original_quality.as_deref(),
                *left_index,
            );
            let right_score = score(
                self.config,
                &state,
                right,
                original_quality.as_deref(),
                *right_index,
            );
            left_score.cmp(&right_score)
        });
        let candidates = indexed
            .into_iter()
            .map(|(_, candidate)| candidate)
            .collect::<Vec<_>>();
        let preferred = candidates.first().ok_or(GatewayError::ModelForbidden)?;
        let preferred_reason = if preferred.provider_key == original.provider_key
            && preferred.provider_model_id == original.provider_model_id
        {
            "registry"
        } else {
            changed_reason(self.config, &state, &original, preferred)
        };
        Ok(RankedCandidates {
            preferred_provider: preferred.provider_key.clone(),
            preferred_model: preferred.provider_model_id.clone(),
            preferred_reason,
            candidates,
        })
    }

    pub fn begin(
        &self,
        provider: &str,
        started_at: DateTime<Utc>,
    ) -> Result<RoutingLease, GatewayError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        let provider_state = state
            .get_mut(provider)
            .ok_or(GatewayError::InvalidConfiguration)?;
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
            active: true,
        })
    }
}

impl RoutingLease {
    pub fn conclude(&mut self, completed_at: DateTime<Utc>) {
        let Ok(mut state) = self.engine.state.lock() else {
            return;
        };
        let Some(provider) = state.get_mut(&self.provider) else {
            return;
        };
        release_active(provider, &mut self.active);
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
    latency_ms: u64,
    active: usize,
    cost: u128,
    original_index: usize,
}

impl Ord for CandidateScore {
    fn cmp(&self, other: &Self) -> Ordering {
        (
            self.quality_group,
            self.latency_ms,
            self.active,
            self.cost,
            self.original_index,
        )
            .cmp(&(
                other.quality_group,
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
) -> CandidateScore {
    let provider = state
        .get(&candidate.provider_key)
        .copied()
        .unwrap_or_default();
    CandidateScore {
        quality_group: u8::from(
            !config.auto_quality && candidate.selector.quality() != original_quality,
        ),
        latency_ms: if config.health_aware {
            latency_score(provider)
        } else {
            0
        },
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
) -> &'static str {
    let original_state = state
        .get(&original.provider_key)
        .copied()
        .unwrap_or_default();
    let selected_state = state
        .get(&selected.provider_key)
        .copied()
        .unwrap_or_default();
    if config.health_aware && latency_score(selected_state) < latency_score(original_state) {
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

fn latency_score(provider: ProviderState) -> u64 {
    provider
        .last_completed_at
        .map_or(u64::MAX, |_| provider.latency_ewma_ms)
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
    fn later_started_result_wins_when_requests_complete_out_of_order() {
        let engine = RoutingEngine::new(RoutingConfig::default(), providers());
        let started = Utc::now();
        let mut old = engine.begin("primary", started).unwrap();
        let mut newer = engine
            .begin("primary", started + Duration::milliseconds(1))
            .unwrap();
        newer.conclude(started + Duration::milliseconds(2));
        old.conclude(started + Duration::milliseconds(3));
        let ranked = engine
            .rank(vec![
                candidate("primary", QualityTier::Advanced, 4),
                candidate("secondary", QualityTier::Standard, 5),
            ])
            .unwrap();
        assert_eq!(ranked.candidates[0].provider_key, "primary");
    }

    #[test]
    fn failed_provider_is_never_excluded() {
        let engine = RoutingEngine::new(RoutingConfig::default(), ["primary".to_owned()]);
        let started = Utc::now();
        let mut failed = engine.begin("primary", started).unwrap();
        failed.conclude(started + Duration::milliseconds(1));
        for _ in 0..3 {
            let ranked = engine
                .rank(vec![candidate("primary", QualityTier::Advanced, 4)])
                .unwrap();
            assert_eq!(ranked.candidates[0].provider_key, "primary");
            let _lease = engine.begin("primary", Utc::now()).unwrap();
        }
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
            },
            providers(),
        );
        let _active = load_engine.begin("primary", started).unwrap();
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
            },
            providers(),
        );
        let _active = quality_guard.begin("primary", started).unwrap();
        assert_eq!(
            quality_guard.rank(candidates).unwrap().candidates[0].provider_key,
            "primary"
        );
    }

    #[test]
    fn measured_health_change_and_reason_use_the_same_score() {
        let engine = RoutingEngine::new(RoutingConfig::default(), providers());
        let started = Utc::now();
        let mut secondary = engine.begin("secondary", started).unwrap();
        secondary.conclude(started + Duration::milliseconds(5));
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
