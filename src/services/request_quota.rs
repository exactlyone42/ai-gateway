use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    adapters::{ValidatedTranslationPlan, validate_for_provider},
    canonical::CanonicalRequest,
    error::GatewayError,
    ledger::{RequestStart, TokenUsage},
    model_registry::{CapabilityRequirements, GatewayOperation},
    policy::value_text,
    quota::{QuotaAdmission, QuotaEstimate, QuotaOutcome, QuotaRejection},
    routing::HalfOpenClaim,
    services::request_policy::PreparedPolicy,
    state::AppState,
};

pub struct PreparedQuota {
    pub admission: QuotaAdmission,
    pub translation: ValidatedTranslationPlan,
    pub routing_reason: &'static str,
    pub routing_claim: Option<HalfOpenClaim>,
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
    let mut candidates = state
        .models
        .candidates(
            prepared.rejection.principal.tenant_id,
            prepared.rejection.principal.is_development_bootstrap(),
            operation,
            prepared.selector.clone(),
            required,
        )
        .await?;
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
        return Err(if had_unsupported_translation {
            GatewayError::UnsupportedRequest
        } else {
            GatewayError::ModelForbidden
        });
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
    let mut ranked = state.routing.rank(candidates)?;
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
    if prepared.rejection.principal.is_development_bootstrap() {
        let selection = candidates
            .into_iter()
            .next()
            .ok_or(GatewayError::ModelForbidden)?;
        selection.record_selected();
        let translation = validate_for_provider(request, &selection)?;
        let routing_reason = ranked.reason_for(&selection, state.routing.quota_aware());
        let routing_claim = ranked.take_claim_for(&selection);
        return Ok(PreparedQuota {
            routing_reason,
            routing_claim,
            admission: QuotaAdmission {
                selection,
                estimated_usage: TokenUsage {
                    input_tokens: input_upper_bound,
                    output_tokens: output_limit.unwrap_or(0).saturating_mul(output_multiplier),
                    ..TokenUsage::default()
                },
                snapshot: None,
                concurrency: Vec::new(),
            },
            translation,
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
            let routing_reason =
                ranked.reason_for(&admission.selection, state.routing.quota_aware());
            let routing_claim = ranked.take_claim_for(&admission.selection);
            Ok(PreparedQuota {
                routing_reason,
                routing_claim,
                admission: *admission,
                translation,
            })
        }
        QuotaOutcome::Rejected(snapshot, rejection) => {
            record_rejection(state, prepared, snapshot).await?;
            Err(match rejection {
                QuotaRejection::HardLimit => GatewayError::QuotaExceeded,
                QuotaRejection::SoftDeferred => GatewayError::QuotaDeferred,
            })
        }
    }
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
    state: &AppState,
    prepared: &PreparedPolicy,
    quota: crate::ledger::RequestQuotaSnapshot,
) -> Result<(), GatewayError> {
    let trace_id = match crate::logging::current_trace_ids().0 {
        value if value.is_empty() => None,
        value => Some(value),
    };
    state
        .ledger
        .reject_with_quota(
            RequestStart {
                request_id: prepared.rejection.request_id.clone(),
                session_id: prepared.rejection.session_id.clone(),
                trace_id,
                requested_model: Some(prepared.requested_model.clone()),
                workload_class: Some(value_text(&prepared.policy.workload_class)),
                urgency: Some(value_text(&prepared.policy.urgency)),
                quality: Some(value_text(&prepared.policy.quality)),
                budget_class: Some(value_text(&prepared.policy.budget_class)),
                estimated_tokens: prepared.rejection.estimated_tokens,
                selection: None,
                policy: Some(prepared.policy.ledger_snapshot()),
                principal: prepared.rejection.principal.clone(),
                created_at: prepared.rejection.received_at,
            },
            prepared.rejection.operation,
            prepared.rejection.client_agent_id.clone(),
            prepared.rejection.parent_agent_id.clone(),
            Some(quota),
        )
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
