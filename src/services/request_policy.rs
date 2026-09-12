use axum::http::HeaderMap;
use chrono::{DateTime, Utc};

use crate::{
    error::GatewayError,
    identity::PrincipalContext,
    ledger::{
        ModelSelectionSnapshot, RequestCompletion, RequestPolicySnapshot, RequestStart, Reservation,
    },
    logging,
    model_registry::ModelSelector,
    policy::{EffectivePolicy, PolicyDenial, value_text},
    state::AppState,
};

#[derive(Clone)]
pub struct RejectionContext {
    pub principal: PrincipalContext,
    pub received_at: DateTime<Utc>,
    pub request_id: String,
    pub session_id: Option<String>,
    pub operation: &'static str,
    pub client_agent_id: Option<String>,
    pub parent_agent_id: Option<String>,
    pub estimated_tokens: Option<i64>,
}

pub struct PreparedPolicy {
    pub policy: EffectivePolicy,
    pub selector: ModelSelector,
    pub requested_model: String,
    pub(crate) rejection: RejectionContext,
}

pub async fn prepare(
    state: &AppState,
    headers: &HeaderMap,
    mut selector: ModelSelector,
    rejection: RejectionContext,
) -> Result<PreparedPolicy, GatewayError> {
    let requested_model = selector.requested_value();
    let policy = state
        .policies
        .evaluate(
            &rejection.principal,
            headers,
            &selector,
            rejection.received_at,
        )
        .await?;
    if policy.denied {
        record(
            state,
            &rejection,
            requested_model,
            &policy,
            policy.ledger_snapshot(),
            None,
        )
        .await?;
        return Err(match policy.denial_kind() {
            Some(PolicyDenial::ModelAllowlistEmpty) => GatewayError::ModelForbidden,
            _ => GatewayError::PolicyDenied,
        });
    }
    if selector.quality().is_none() && !policy.allows_model(&requested_model) {
        record(
            state,
            &rejection,
            requested_model,
            &policy,
            policy.rejection_snapshot("model_not_allowed"),
            None,
        )
        .await?;
        return Err(GatewayError::ModelForbidden);
    }
    if selector.quality().is_some() {
        selector = ModelSelector::parse(&policy.quality_selector())?;
    }
    Ok(PreparedPolicy {
        policy,
        selector,
        requested_model,
        rejection,
    })
}

pub async fn enforce_selected_model(
    state: &AppState,
    prepared: &PreparedPolicy,
    selected_model: &str,
    selection: Option<ModelSelectionSnapshot>,
) -> Result<(), GatewayError> {
    if prepared.policy.allows_model(selected_model) {
        return Ok(());
    }
    record(
        state,
        &prepared.rejection,
        prepared.requested_model.clone(),
        &prepared.policy,
        prepared.policy.rejection_snapshot("model_not_allowed"),
        selection,
    )
    .await?;
    Err(GatewayError::ModelForbidden)
}

/// Records a request rejected before any model was selected.
///
/// The policy and deadline rejections above already leave a row, so without
/// this one the single case an operator meets most often -- a registry with
/// nothing to offer, right after a deployment -- is the one case Admin can
/// tell them nothing about.
pub async fn record_rejected(
    state: &AppState,
    prepared: &PreparedPolicy,
    reason: &str,
) -> Result<(), GatewayError> {
    record(
        state,
        &prepared.rejection,
        prepared.requested_model.clone(),
        &prepared.policy,
        prepared.policy.rejection_snapshot(reason),
        None,
    )
    .await
}

pub async fn enforce_deadline(
    mut reservation: Reservation,
    policy: &EffectivePolicy,
) -> Result<Reservation, GatewayError> {
    if !policy.is_expired(Utc::now()) {
        return Ok(reservation);
    }
    reservation.annotate_policy(policy.rejection_snapshot("deadline_expired"));
    reservation
        .finish(RequestCompletion {
            status: "rejected".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: Vec::new(),
            completed_at: Utc::now(),
        })
        .await?;
    Err(GatewayError::PolicyDenied)
}

/// Records a request rejected before the policy could even be evaluated: the
/// body did not decode, or the model name is not a name.
///
/// These requests already carry an identity and a request id, so Admin can
/// answer "it arrived, and here is why it was refused" instead of nothing at
/// all. There is no policy to snapshot yet, which is why this does not go
/// through [`record`].
pub async fn record_unprocessable(
    state: &AppState,
    context: &RejectionContext,
    requested_model: Option<String>,
    reason: &str,
) -> Result<(), GatewayError> {
    let trace_id = match logging::current_trace_ids().0 {
        value if value.is_empty() => None,
        value => Some(value),
    };
    state
        .ledger
        .reject(
            RequestStart {
                request_id: context.request_id.clone(),
                session_id: context.session_id.clone(),
                trace_id,
                requested_model,
                workload_class: None,
                urgency: None,
                quality: None,
                budget_class: None,
                estimated_tokens: context.estimated_tokens,
                selection: None,
                policy: Some(RequestPolicySnapshot::unevaluated(reason)),
                principal: context.principal.clone(),
                created_at: context.received_at,
            },
            context.operation,
            context.client_agent_id.clone(),
            context.parent_agent_id.clone(),
        )
        .await?;
    Ok(())
}

async fn record(
    state: &AppState,
    context: &RejectionContext,
    requested_model: String,
    policy: &EffectivePolicy,
    snapshot: RequestPolicySnapshot,
    selection: Option<ModelSelectionSnapshot>,
) -> Result<(), GatewayError> {
    let trace_id = match logging::current_trace_ids().0 {
        value if value.is_empty() => None,
        value => Some(value),
    };
    state
        .ledger
        .reject(
            RequestStart {
                request_id: context.request_id.clone(),
                session_id: context.session_id.clone(),
                trace_id,
                requested_model: Some(requested_model),
                workload_class: Some(value_text(&policy.workload_class)),
                urgency: Some(value_text(&policy.urgency)),
                quality: Some(value_text(&policy.quality)),
                budget_class: Some(value_text(&policy.budget_class)),
                estimated_tokens: context.estimated_tokens,
                selection,
                policy: Some(snapshot),
                principal: context.principal.clone(),
                created_at: context.received_at,
            },
            context.operation,
            context.client_agent_id.clone(),
            context.parent_agent_id.clone(),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::enforce_deadline;
    use crate::{
        error::GatewayError,
        identity::PrincipalContext,
        ledger::{Ledger, RequestStart},
        policy::effective_policy_for_deadline,
    };

    #[sqlx::test]
    async fn an_expired_deadline_is_durably_rejected_at_the_provider_gate(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let directory = std::env::temp_dir().join(format!("policy-deadline-{}", Uuid::new_v4()));
        let ledger = Ledger::open(pool.clone(), directory.clone(), 1024 * 1024, 32 * 1024).unwrap();
        let deadline = Utc::now()
            .checked_sub_signed(Duration::milliseconds(1))
            .unwrap();
        let policy = effective_policy_for_deadline(deadline).unwrap();
        assert!(!policy.denied);
        let reservation = ledger
            .reserve(RequestStart {
                request_id: "deadline-test".to_owned(),
                session_id: None,
                trace_id: None,
                requested_model: Some("model".to_owned()),
                workload_class: None,
                urgency: None,
                quality: None,
                budget_class: None,
                estimated_tokens: None,
                selection: None,
                policy: Some(policy.ledger_snapshot()),
                principal: PrincipalContext {
                    tenant_id,
                    credential_id: Uuid::nil(),
                    credential_name: "deadline".to_owned(),
                    user_id: None,
                    user_name: None,
                    device_id: None,
                    device_name: None,
                    client_id: None,
                    client_name: None,
                    agent_id: None,
                    agent_name: None,
                },
                created_at: Utc::now(),
            })
            .await
            .unwrap();
        assert!(matches!(
            enforce_deadline(reservation, &policy).await,
            Err(GatewayError::PolicyDenied)
        ));
        ledger.wait_for_idle().await;
        ledger.flush_startup().await.unwrap();
        let evidence: serde_json::Value = sqlx::query_scalar(
            "SELECT jsonb_build_object('status', status, 'decision', policy_decision) FROM gateway_requests WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(evidence["status"], "rejected");
        assert_eq!(evidence["decision"]["denied"], true);
        assert_eq!(evidence["decision"]["denial_reason"], "deadline_expired");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
