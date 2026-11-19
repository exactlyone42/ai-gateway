use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    adapters::{ValidatedTranslationPlan, validate_for_provider},
    canonical::CanonicalRequest,
    error::GatewayError,
    ledger::{RequestCompletion, RequestStart, Reservation, TokenUsage},
    model_registry::{CapabilityRequirements, GatewayOperation},
    policy::value_text,
    quota::{QuotaAdmission, QuotaEstimate, QuotaOutcome, QuotaRejection},
    services::request_policy::PreparedPolicy,
    state::AppState,
    task_classification::{self, ClassifierMode},
};

pub struct PreparedQuota {
    pub admission: QuotaAdmission,
    pub translation: ValidatedTranslationPlan,
    pub reservation: Reservation,
}

#[derive(Clone, Copy)]
struct RoutingMeasurement {
    classification_elapsed_us: Option<u64>,
    jev_attempted: bool,
    jev_elapsed_ms: Option<u64>,
}

pub async fn admit(
    state: &AppState,
    prepared: &PreparedPolicy,
    gateway_request_id: Uuid,
    request: &CanonicalRequest,
    body_bytes: usize,
    required: CapabilityRequirements,
) -> Result<PreparedQuota, GatewayError> {
    let operation = request.source_operation();
    let body = request.source_document();
    let mut candidates = match state
        .models
        .candidates(
            prepared.rejection.principal.tenant_id,
            prepared.rejection.principal.is_development_bootstrap(),
            operation,
            prepared.selector.clone(),
            required,
        )
        .await
    {
        Ok(candidates) => candidates,
        Err(error) => {
            let error = GatewayError::from(error);
            // "The registry has nothing for this request" is the rejection an
            // operator meets most often, and every other rejection already
            // leaves a row. A registry that is unavailable is a different
            // thing: that is the gateway failing, not the request, and it
            // keeps its own error rather than a denial reason.
            if matches!(error, GatewayError::ModelForbidden) {
                super::request_policy::record_rejected(state, prepared, "no_model_available")
                    .await?;
            }
            return Err(error);
        }
    };
    let mut had_unsupported_translation = false;
    candidates.retain(
        |candidate| match validate_for_provider(request, candidate) {
            Ok(_) => true,
            Err(GatewayError::UnsupportedRequest) => {
                had_unsupported_translation = true;
                false
            }
            Err(_) => false,
        },
    );
    if candidates.is_empty() {
        // Two different outcomes, so two different reasons: an operator
        // reading the ledger needs to know whether the registry had nothing
        // at all or had something this request could not be translated for.
        let (reason, error) = if had_unsupported_translation {
            ("unsupported_translation", GatewayError::UnsupportedRequest)
        } else {
            ("no_model_for_request", GatewayError::ModelForbidden)
        };
        super::request_policy::record_rejected(state, prepared, reason).await?;
        return Err(error);
    }
    let policy_reference = candidates
        .first()
        .cloned()
        .ok_or(GatewayError::ModelForbidden)?;
    candidates.retain(|candidate| prepared.policy.allows_model(&candidate.public_model_id));
    if candidates.is_empty() {
        super::request_policy::enforce_selected_model(
            state,
            prepared,
            &policy_reference.public_model_id,
            policy_reference.ledger_snapshot(),
        )
        .await?;
        return Err(GatewayError::ModelForbidden);
    }
    let subscription_only = candidates
        .iter()
        .any(|candidate| candidate.profile.is_codex_subscription());
    if subscription_only {
        candidates.retain(|candidate| candidate.profile.is_codex_subscription());
    }
    let original_candidates = candidates.clone();
    let classification_started = std::time::Instant::now();
    let quality_requested = prepared.selector.quality().is_some();
    let task_decision = if quality_requested
        && !subscription_only
        && operation != GatewayOperation::AnthropicCountTokens
    {
        match tokio::time::timeout(
            std::time::Duration::from_millis(50),
            task_classification::decide(
                &state.pool,
                prepared.rejection.principal.tenant_id,
                body,
                &candidates,
            ),
        )
        .await
        {
            Ok(Ok(decision)) => decision,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "task classification failed; preserving original model");
                None
            }
            Err(_) => {
                tracing::warn!("task classification timed out; preserving original model");
                None
            }
        }
    } else {
        None
    };
    let classification_elapsed_us = task_decision.as_ref().map(|_| {
        classification_started
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64
    });
    if let Some(decision) = &task_decision {
        tracing::info!(
            task = decision.classification.task,
            confidence = decision.classification.confidence,
            reason = decision.classification.reason,
            profile_id = ?decision.profile_id,
            profile_version = ?decision.profile_version,
            mode = ?decision.mode,
            "task classification produced a capable model shortlist"
        );
        if decision.mode == ClassifierMode::Enforced {
            candidates = decision.candidates.clone();
        }
    }
    let mut jev_attempted = false;
    let mut jev_elapsed_ms = None;
    let routing_reason = if quality_requested
        && !subscription_only
        && operation != GatewayOperation::AnthropicCountTokens
        && let Some(router) = &state.jev
    {
        jev_attempted = true;
        let jev_started = std::time::Instant::now();
        let mut routing_candidates = candidates;
        routing_candidates.retain(|candidate| {
            prepared.policy.allows_model(&candidate.public_model_id)
                && validate_for_provider(request, candidate).is_ok()
                && task_decision.as_ref().is_none_or(|decision| {
                    decision.mode != ClassifierMode::Enforced
                        || decision
                            .candidates
                            .iter()
                            .any(|shortlisted| shortlisted.model_id == candidate.model_id)
                })
        });
        let reason = if routing_candidates.is_empty() {
            None
        } else {
            router
                .select(&state.client, body, &mut routing_candidates)
                .await
        };
        jev_elapsed_ms = Some(jev_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        if reason.is_some() {
            candidates = routing_candidates;
        } else {
            candidates = original_candidates;
        }
        reason
    } else {
        None
    };
    let measurement = RoutingMeasurement {
        classification_elapsed_us,
        jev_attempted,
        jev_elapsed_ms,
    };
    let ranked = state.routing.rank(candidates)?;
    let candidates = ranked.candidates.clone();
    let first = candidates.first().ok_or(GatewayError::ModelForbidden)?;
    super::request_policy::enforce_selected_model(
        state,
        prepared,
        &first.public_model_id,
        first.ledger_snapshot(),
    )
    .await?;
    let output_limit = requested_output_tokens(operation, body);
    let output_multiplier = output_multiplier(operation, body)?;
    let billable = operation != GatewayOperation::AnthropicCountTokens;
    let input_upper_bound = if billable {
        u64::try_from(body_bytes).unwrap_or(u64::MAX)
    } else {
        0
    };
    let context = &prepared.rejection;
    let mut reservation = state
        .ledger
        .reserve_with_quota(
            RequestStart {
                request_id: context.request_id.clone(),
                session_id: context.session_id.clone(),
                trace_id: request.trace_id.clone(),
                requested_model: Some(prepared.requested_model.clone()),
                workload_class: Some(value_text(&prepared.policy.workload_class)),
                urgency: Some(value_text(&prepared.policy.urgency)),
                quality: Some(value_text(&prepared.policy.quality)),
                budget_class: Some(value_text(&prepared.policy.budget_class)),
                estimated_tokens: context.estimated_tokens,
                selection: None,
                policy: Some(prepared.policy.ledger_snapshot()),
                principal: context.principal.clone(),
                created_at: context.received_at,
            },
            Some(gateway_request_id),
            None,
            Some(&context.attribution),
            Some((
                context.operation,
                context.client_agent_id.clone(),
                context.parent_agent_id.clone(),
            )),
        )
        .await?;
    reservation.cancel_on_force(state.lifecycle.force_token());
    if prepared.rejection.principal.is_development_bootstrap() {
        let selection = candidates
            .into_iter()
            .next()
            .ok_or(GatewayError::ModelForbidden)?;
        selection.record_selected();
        let translation = validate_for_provider(request, &selection)?;
        let routing_reason = routing_reason
            .unwrap_or_else(|| ranked.reason_for(&selection, state.routing.quota_aware()));
        let task_routing = task_routing_snapshot(
            &task_decision,
            required,
            prepared,
            routing_reason,
            &selection.public_model_id,
            measurement,
        );
        let admission = QuotaAdmission {
            selection,
            estimated_usage: TokenUsage {
                input_tokens: input_upper_bound,
                output_tokens: output_limit.unwrap_or(0).saturating_mul(output_multiplier),
                ..TokenUsage::default()
            },
            snapshot: None,
            concurrency: Vec::new(),
        };
        reservation.annotate_admission(
            billable.then(|| estimated_tokens(&admission.estimated_usage)),
            None,
        );
        if let Some(selection) = admission.selection.ledger_snapshot() {
            reservation.annotate_selection(&selection);
        }
        reservation.annotate_policy(
            prepared
                .policy
                .ledger_snapshot()
                .with_routing(routing_reason)
                .with_task_routing(task_routing),
        );
        return Ok(PreparedQuota {
            admission,
            translation,
            reservation,
        });
    }
    match state
        .quotas
        .admit_with_soft_routing(
            gateway_request_id,
            &prepared.rejection.principal,
            candidates,
            QuotaEstimate {
                input_upper_bound,
                requested_output_tokens: output_limit,
                output_multiplier,
                billable,
                unbounded_input: required.images,
                requires_extended_pricing: request_uses_cache(body),
                unsupported_cost_features: required.tools || body.get("service_tier").is_some(),
            },
            Utc::now(),
            state.routing.quota_aware(),
        )
        .await?
    {
        QuotaOutcome::Allowed(admission) => {
            admission.selection.record_selected();
            let translation = validate_for_provider(request, &admission.selection)?;
            let routing_reason = routing_reason.unwrap_or_else(|| {
                ranked.reason_for(&admission.selection, state.routing.quota_aware())
            });
            let task_routing = task_routing_snapshot(
                &task_decision,
                required,
                prepared,
                routing_reason,
                &admission.selection.public_model_id,
                measurement,
            );
            reservation.annotate_admission(
                billable.then(|| estimated_tokens(&admission.estimated_usage)),
                admission.snapshot.clone(),
            );
            if let Some(selection) = admission.selection.ledger_snapshot() {
                reservation.annotate_selection(&selection);
            }
            reservation.annotate_policy(
                prepared
                    .policy
                    .ledger_snapshot()
                    .with_routing(routing_reason)
                    .with_task_routing(task_routing),
            );
            Ok(PreparedQuota {
                admission: *admission,
                translation,
                reservation,
            })
        }
        QuotaOutcome::Rejected(snapshot, rejection) => {
            record_rejection(reservation, prepared.rejection.estimated_tokens, snapshot).await?;
            Err(match rejection {
                QuotaRejection::HardLimit => GatewayError::QuotaExceeded,
                QuotaRejection::SoftDeferred => GatewayError::QuotaDeferred,
            })
        }
    }
}

fn task_routing_snapshot(
    decision: &Option<task_classification::TaskRoutingDecision>,
    required: CapabilityRequirements,
    prepared: &PreparedPolicy,
    routing_reason: &str,
    final_model: &str,
    measurement: RoutingMeasurement,
) -> Value {
    let classification = decision.as_ref().map(|decision| {
        serde_json::json!({
            "taxonomy_version": "task-taxonomy-v1",
            "task": decision.classification.task,
            "confidence": decision.classification.confidence,
            "reason": decision.classification.reason,
            "mode": match decision.mode {
                ClassifierMode::Shadow => "shadow",
                ClassifierMode::Enforced => "enforced",
            },
            "profile_id": decision.profile_id,
            "profile_version": decision.profile_version,
            "shortlist": decision.candidates.iter().take(16).map(|candidate| candidate.public_model_id.chars().take(128).collect::<String>()).collect::<Vec<_>>(),
            "elapsed_us": measurement.classification_elapsed_us,
        })
    });
    serde_json::json!({
        "classification": classification,
        "capability_version": "model-capabilities-v1",
        "required_capabilities": {
            "streaming": required.streaming,
            "tools": required.tools,
            "reasoning": required.reasoning,
            "images": required.images,
            "structured_output": required.structured_output,
        },
        "service_objective_version": "service-objective-v1",
        "service_objective": {
            "workload_class": value_text(&prepared.policy.workload_class),
            "urgency": value_text(&prepared.policy.urgency),
            "quality": value_text(&prepared.policy.quality),
            "budget_class": value_text(&prepared.policy.budget_class),
        },
        "original_model": prepared.requested_model,
        "jev_attempted": measurement.jev_attempted,
        "jev_elapsed_ms": measurement.jev_elapsed_ms,
        "jev_opinion": (routing_reason == "jev").then_some(final_model),
        "final_model": final_model,
        "selection_reason": routing_reason,
        "changed": prepared.requested_model != final_model,
    })
}

fn output_multiplier(operation: GatewayOperation, body: &Value) -> Result<u64, GatewayError> {
    if operation != GatewayOperation::OpenAiChatCompletions {
        return Ok(1);
    }
    body.get("n")
        .map_or(Some(1), Value::as_u64)
        .filter(|value| (1..=1024).contains(value))
        .ok_or(GatewayError::UnsupportedRequest)
}

fn request_uses_cache(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key("cache_control") || object.values().any(request_uses_cache)
        }
        Value::Array(values) => values.iter().any(request_uses_cache),
        _ => false,
    }
}

async fn record_rejection(
    mut reservation: Reservation,
    estimated_tokens: Option<i64>,
    quota: crate::ledger::RequestQuotaSnapshot,
) -> Result<(), GatewayError> {
    reservation.annotate_admission(estimated_tokens, Some(quota));
    reservation
        .finish(RequestCompletion {
            status: "rejected".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: Vec::new(),
            completed_at: Utc::now(),
        })
        .await?;
    Ok(())
}

fn requested_output_tokens(operation: GatewayOperation, body: &Value) -> Option<u64> {
    match operation {
        GatewayOperation::AnthropicCountTokens => Some(0),
        GatewayOperation::AnthropicMessages => body.get("max_tokens").and_then(Value::as_u64),
        GatewayOperation::OpenAiResponses => body.get("max_output_tokens").and_then(Value::as_u64),
        GatewayOperation::OpenAiChatCompletions => body
            .get("max_completion_tokens")
            .or_else(|| body.get("max_tokens"))
            .and_then(Value::as_u64),
        GatewayOperation::GeminiGenerateContent => None,
    }
}

pub fn estimated_tokens(usage: &TokenUsage) -> i64 {
    usage
        .input_tokens
        .saturating_add(usage.output_tokens)
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{output_multiplier, request_uses_cache};
    use crate::{error::GatewayError, model_registry::GatewayOperation};

    #[test]
    fn chat_choice_count_is_a_checked_output_multiplier() {
        assert_eq!(
            output_multiplier(GatewayOperation::OpenAiChatCompletions, &json!({"n": 4})).unwrap(),
            4
        );
        assert!(matches!(
            output_multiplier(GatewayOperation::OpenAiChatCompletions, &json!({"n": 0})),
            Err(GatewayError::UnsupportedRequest)
        ));
    }

    #[test]
    fn nested_cache_controls_require_extended_pricing() {
        assert!(request_uses_cache(&json!({
            "messages": [{"content": [{"text": "x", "cache_control": {"type": "ephemeral"}}]}]
        })));
        assert!(!request_uses_cache(
            &json!({"messages": [{"content": "x"}]})
        ));
    }
}
