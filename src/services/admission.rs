use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use tracing::Instrument as _;

use crate::{
    error::GatewayError,
    identity::PrincipalContext,
    ledger::{RequestCompletion, Reservation},
    model_registry::ModelSelection,
    policy::EffectivePolicy,
    quota::QuotaConcurrencyConstraint,
    scheduler::{CacheAffinity, SchedulerRequest},
    state::AppState,
};

pub struct AdmissionContext<'a> {
    pub principal: &'a PrincipalContext,
    pub policy: &'a EffectivePolicy,
    pub selection: &'a ModelSelection,
    pub quota: Vec<QuotaConcurrencyConstraint>,
    pub body: &'a Value,
    pub received_at: DateTime<Utc>,
}

pub async fn acquire(
    state: &AppState,
    mut reservation: Reservation,
    input: AdmissionContext<'_>,
) -> Result<Reservation, GatewayError> {
    let AdmissionContext {
        principal,
        policy,
        selection,
        quota,
        body,
        received_at,
    } = input;
    let request = SchedulerRequest {
        tenant_id: principal.tenant_id,
        provider_id: selection.provider_id,
        model_id: selection.model_id,
        user_id: principal.user_id,
        device_id: principal.device_id,
        client_id: principal.client_id,
        agent_id: principal.agent_id,
        credential_id: principal.credential_id,
        workload: policy.workload_class,
        quality: crate::policy::value_text(&policy.quality),
        received_at,
        policy_deadline: policy.deadline_at,
        cache_affinity: cache_affinity(body, received_at),
        quota,
    };
    match state.scheduler.try_acquire(&request) {
        Ok(Some(lease)) => {
            reservation.attach_scheduler_lease(lease);
            return Ok(reservation);
        }
        Ok(None) => {}
        Err(error) => return reject_unqueued(reservation, error).await,
    }

    let effective_deadline = state.scheduler.effective_deadline(&request)?;
    reservation.set_deadline_at(effective_deadline);
    let entered_at = Utc::now();
    reservation.checkpoint_queue_entered(entered_at).await?;
    let ticket = match state.scheduler.enqueue(request) {
        Ok(ticket) => ticket,
        Err(error) => return reject(reservation, error).await,
    };
    let span = tracing::info_span!(
        "scheduler.queue",
        scheduler.workload = %crate::policy::value_text(&policy.workload_class),
        scheduler.outcome = tracing::field::Empty,
    );
    let lease = match ticket.wait().instrument(span.clone()).await {
        Ok(lease) => {
            span.record("scheduler.outcome", "accepted");
            lease
        }
        Err(error) => {
            span.record("scheduler.outcome", "rejected");
            return reject(reservation, error).await;
        }
    };
    let left_at = Utc::now();
    reservation.checkpoint_queue_left(left_at).await?;
    if Utc::now() >= effective_deadline {
        drop(lease);
        return reject_after_queue_left(reservation, crate::scheduler::SchedulerError::Expired)
            .await;
    }
    reservation.attach_scheduler_lease(lease);
    Ok(reservation)
}

async fn reject(
    mut reservation: Reservation,
    error: crate::scheduler::SchedulerError,
) -> Result<Reservation, GatewayError> {
    if error == crate::scheduler::SchedulerError::HardQuota {
        reservation.annotate_quota_scheduler_outcome("hard_concurrency_rejected");
    }
    reservation.checkpoint_queue_left(Utc::now()).await?;
    reservation
        .finish(RequestCompletion {
            status: "rejected".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: Vec::new(),
            completed_at: Utc::now(),
        })
        .await?;
    Err(error.into())
}

async fn reject_unqueued(
    mut reservation: Reservation,
    error: crate::scheduler::SchedulerError,
) -> Result<Reservation, GatewayError> {
    if error == crate::scheduler::SchedulerError::HardQuota {
        reservation.annotate_quota_scheduler_outcome("hard_concurrency_rejected");
    }
    reservation
        .finish(RequestCompletion {
            status: "rejected".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: Vec::new(),
            completed_at: Utc::now(),
        })
        .await?;
    Err(error.into())
}

async fn reject_after_queue_left(
    reservation: Reservation,
    error: crate::scheduler::SchedulerError,
) -> Result<Reservation, GatewayError> {
    reservation
        .finish(RequestCompletion {
            status: "rejected".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: Vec::new(),
            completed_at: Utc::now(),
        })
        .await?;
    Err(error.into())
}

fn cache_affinity(value: &Value, received_at: DateTime<Utc>) -> Option<CacheAffinity> {
    fn ttl(value: &Value) -> Option<Duration> {
        match value {
            Value::Object(object) => {
                if let Some(control) = object.get("cache_control") {
                    let duration = control.get("ttl").and_then(Value::as_str).map_or(
                        Duration::minutes(5),
                        |value| match value {
                            "1h" => Duration::hours(1),
                            _ => Duration::minutes(5),
                        },
                    );
                    return Some(duration);
                }
                object.values().filter_map(ttl).min()
            }
            Value::Array(values) => values.iter().filter_map(ttl).min(),
            _ => None,
        }
    }
    ttl(value).map(|ttl| CacheAffinity {
        expires_at: received_at + ttl,
        source: "protocol_cache_control",
    })
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use serde_json::json;

    use super::cache_affinity;

    #[test]
    fn cache_expiry_uses_only_bounded_protocol_ttls() {
        let at = Utc::now();
        let short = cache_affinity(&json!({"cache_control": {"type": "ephemeral"}}), at).unwrap();
        let long = cache_affinity(
            &json!({"cache_control": {"type": "ephemeral", "ttl": "1h"}}),
            at,
        )
        .unwrap();
        let forged = cache_affinity(&json!({"cache_control": {"ttl": "999h"}}), at).unwrap();
        assert_eq!(short.expires_at, at + Duration::minutes(5));
        assert_eq!(long.expires_at, at + Duration::hours(1));
        assert_eq!(forged.expires_at, at + Duration::minutes(5));
    }
}
