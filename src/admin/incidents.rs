use std::net::IpAddr;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row as _;
use uuid::Uuid;

use super::{AdminContext, AdminError, AdminService, AuditEntry, append_audit};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentListQuery {
    pub status: Option<String>,
    pub service: Option<String>,
    pub impact_scope: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentCreate {
    pub title: String,
    pub status: String,
    pub service: String,
    pub impact_scope: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub summary: String,
    pub impact: String,
    pub root_cause: Option<String>,
    pub temporary_fix: Option<String>,
    pub permanent_fix: Option<String>,
    pub prevention: Option<String>,
    pub validation_evidence: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentPatch {
    pub version: i64,
    pub status: Option<String>,
    pub ended_at: Option<DateTime<Utc>>,
    pub summary: Option<String>,
    pub impact: Option<String>,
    pub root_cause: Option<String>,
    pub temporary_fix: Option<String>,
    pub permanent_fix: Option<String>,
    pub prevention: Option<String>,
    pub validation_evidence: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentEventCreate {
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    pub message: Option<String>,
    pub gateway_request_id: Option<Uuid>,
    pub deployment_version: Option<String>,
}

impl AdminService {
    pub async fn list_incidents(
        &self,
        context: &AdminContext,
        query: IncidentListQuery,
    ) -> Result<Value, AdminError> {
        validate_optional_enum(query.status.as_deref(), &STATUSES)?;
        validate_optional_enum(query.impact_scope.as_deref(), &SCOPES)?;
        let rows = sqlx::query(
            r#"
            SELECT to_jsonb(i) AS incident
            FROM incidents i
            WHERE tenant_id = $1
              AND ($2::text IS NULL OR status = $2)
              AND ($3::text IS NULL OR service = $3)
              AND ($4::text IS NULL OR impact_scope = $4)
              AND ($5::timestamptz IS NULL OR started_at >= $5)
              AND ($6::timestamptz IS NULL OR started_at <= $6)
            ORDER BY started_at DESC, id DESC
            LIMIT 100
            "#,
        )
        .bind(context.tenant_id)
        .bind(query.status)
        .bind(query.service)
        .bind(query.impact_scope)
        .bind(query.from)
        .bind(query.until)
        .fetch_all(&self.pool)
        .await?;
        let items = rows
            .into_iter()
            .map(|row| row.try_get::<Value, _>("incident"))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({ "items": items }))
    }

    pub async fn get_incident(
        &self,
        context: &AdminContext,
        id: Uuid,
    ) -> Result<Value, AdminError> {
        let incident = sqlx::query_scalar::<_, Value>(
            "SELECT to_jsonb(i) FROM incidents i WHERE tenant_id = $1 AND id = $2",
        )
        .bind(context.tenant_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AdminError::NotFound)?;
        let events = sqlx::query_scalar::<_, Value>(
            "SELECT to_jsonb(e) FROM incident_events e WHERE tenant_id = $1 AND incident_id = $2 ORDER BY occurred_at, id",
        )
        .bind(context.tenant_id)
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(json!({ "incident": incident, "events": events }))
    }

    pub async fn create_incident(
        &self,
        context: &AdminContext,
        input: IncidentCreate,
        source_ip: IpAddr,
        peer_ip: IpAddr,
        request_id: &str,
    ) -> Result<Value, AdminError> {
        validate_create(&input)?;
        let id = Uuid::new_v4();
        let mut transaction = self.pool.begin().await?;
        let incident = sqlx::query_scalar::<_, Value>(
            r#"
            INSERT INTO incidents (
                id, tenant_id, title, status, service, impact_scope, started_at, ended_at,
                summary, impact, root_cause, temporary_fix, permanent_fix, prevention,
                validation_evidence, created_by, updated_by
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$16)
            RETURNING to_jsonb(incidents)
            "#,
        )
        .bind(id)
        .bind(context.tenant_id)
        .bind(&input.title)
        .bind(&input.status)
        .bind(&input.service)
        .bind(&input.impact_scope)
        .bind(input.started_at)
        .bind(input.ended_at)
        .bind(&input.summary)
        .bind(&input.impact)
        .bind(&input.root_cause)
        .bind(&input.temporary_fix)
        .bind(&input.permanent_fix)
        .bind(&input.prevention)
        .bind(&input.validation_evidence)
        .bind(context.account_id)
        .fetch_one(&mut *transaction)
        .await?;
        insert_event(
            &mut transaction,
            context,
            id,
            "created",
            input.started_at,
            Some("Incident created"),
            None,
            Some(&incident),
            None,
            None,
        )
        .await?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: "incident.create",
                target_type: "incident",
                target_id: Some(id),
                before: None,
                after: Some(incident.clone()),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(incident)
    }

    pub async fn update_incident(
        &self,
        context: &AdminContext,
        id: Uuid,
        input: IncidentPatch,
        source_ip: IpAddr,
        peer_ip: IpAddr,
        request_id: &str,
    ) -> Result<Value, AdminError> {
        validate_patch(&input)?;
        let mut transaction = self.pool.begin().await?;
        let before = sqlx::query_scalar::<_, Value>(
            "SELECT to_jsonb(i) FROM incidents i WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(context.tenant_id)
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(AdminError::NotFound)?;
        let after = sqlx::query_scalar::<_, Value>(
            r#"
            UPDATE incidents SET
                status = COALESCE($4, status), ended_at = COALESCE($5, ended_at),
                summary = COALESCE($6, summary), impact = COALESCE($7, impact),
                root_cause = COALESCE($8, root_cause), temporary_fix = COALESCE($9, temporary_fix),
                permanent_fix = COALESCE($10, permanent_fix), prevention = COALESCE($11, prevention),
                validation_evidence = COALESCE($12, validation_evidence), updated_by = $3,
                updated_at = now(), version = version + 1
            WHERE tenant_id = $1 AND id = $2 AND version = $13
            RETURNING to_jsonb(incidents)
            "#,
        )
        .bind(context.tenant_id).bind(id).bind(context.account_id).bind(&input.status)
        .bind(input.ended_at).bind(&input.summary).bind(&input.impact).bind(&input.root_cause)
        .bind(&input.temporary_fix).bind(&input.permanent_fix).bind(&input.prevention)
        .bind(&input.validation_evidence).bind(input.version)
        .fetch_optional(&mut *transaction).await?.ok_or(AdminError::Conflict)?;
        let event_type = if input.status.is_some() {
            "status_changed"
        } else {
            "analysis"
        };
        insert_event(
            &mut transaction,
            context,
            id,
            event_type,
            Utc::now(),
            Some("Incident updated"),
            Some(&before),
            Some(&after),
            None,
            None,
        )
        .await?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: "incident.update",
                target_type: "incident",
                target_id: Some(id),
                before: Some(before),
                after: Some(after.clone()),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(after)
    }

    pub async fn append_incident_event(
        &self,
        context: &AdminContext,
        incident_id: Uuid,
        input: IncidentEventCreate,
        source_ip: IpAddr,
        peer_ip: IpAddr,
        request_id: &str,
    ) -> Result<Value, AdminError> {
        validate_event(&input)?;
        let mut transaction = self.pool.begin().await?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM incidents WHERE tenant_id = $1 AND id = $2)",
        )
        .bind(context.tenant_id)
        .bind(incident_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !exists {
            return Err(AdminError::NotFound);
        }
        if let Some(linked) = input.gateway_request_id {
            let request_exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM gateway_requests WHERE tenant_id = $1 AND id = $2)",
            )
            .bind(context.tenant_id)
            .bind(linked)
            .fetch_one(&mut *transaction)
            .await?;
            if !request_exists {
                return Err(AdminError::NotFound);
            }
        }
        let event = insert_event(
            &mut transaction,
            context,
            incident_id,
            &input.event_type,
            input.occurred_at,
            input.message.as_deref(),
            None,
            None,
            input.gateway_request_id,
            input.deployment_version.as_deref(),
        )
        .await?;
        append_audit(
            &mut transaction,
            AuditEntry {
                tenant_id: context.tenant_id,
                actor_id: context.account_id,
                role: context.role,
                action: "incident.event.append",
                target_type: "incident",
                target_id: Some(incident_id),
                before: None,
                after: Some(event.clone()),
                source_ip,
                peer_ip,
                request_id,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(event)
    }
}

const STATUSES: [&str; 4] = ["investigating", "mitigated", "resolved", "closed"];
const SCOPES: [&str; 3] = ["system", "tenant", "principal"];
const EVENT_TYPES: [&str; 5] = [
    "timeline",
    "analysis",
    "remediation",
    "verification",
    "link",
];

fn validate_create(input: &IncidentCreate) -> Result<(), AdminError> {
    validate_optional_enum(Some(&input.status), &STATUSES)?;
    validate_optional_enum(Some(&input.impact_scope), &SCOPES)?;
    validate_texts([
        Some(&input.title),
        Some(&input.service),
        Some(&input.summary),
        Some(&input.impact),
        input.root_cause.as_ref(),
        input.temporary_fix.as_ref(),
        input.permanent_fix.as_ref(),
        input.prevention.as_ref(),
        input.validation_evidence.as_ref(),
    ])
}

fn validate_patch(input: &IncidentPatch) -> Result<(), AdminError> {
    if input.version < 1 {
        return Err(AdminError::InvalidRequest);
    }
    validate_optional_enum(input.status.as_deref(), &STATUSES)?;
    validate_texts([
        input.summary.as_ref(),
        input.impact.as_ref(),
        input.root_cause.as_ref(),
        input.temporary_fix.as_ref(),
        input.permanent_fix.as_ref(),
        input.prevention.as_ref(),
        input.validation_evidence.as_ref(),
    ])
}

fn validate_event(input: &IncidentEventCreate) -> Result<(), AdminError> {
    validate_optional_enum(Some(&input.event_type), &EVENT_TYPES)?;
    if input.message.is_none()
        && input.gateway_request_id.is_none()
        && input.deployment_version.is_none()
    {
        return Err(AdminError::InvalidRequest);
    }
    validate_texts([input.message.as_ref(), input.deployment_version.as_ref()])
}

fn validate_optional_enum(value: Option<&str>, allowed: &[&str]) -> Result<(), AdminError> {
    if value.is_some_and(|candidate| !allowed.contains(&candidate)) {
        return Err(AdminError::InvalidRequest);
    }
    Ok(())
}

fn validate_texts<'a>(
    values: impl IntoIterator<Item = Option<&'a String>>,
) -> Result<(), AdminError> {
    for value in values.into_iter().flatten() {
        let lower = value.to_ascii_lowercase();
        if value.trim().is_empty()
            || [
                "authorization:",
                "bearer ",
                "api_key",
                "api key",
                "access_token",
                "refresh_token",
                "prompt:",
                "response:",
                "\"prompt\"",
                "\"response\"",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
            || lower
                .split(|character: char| character.is_whitespace() || character == '"')
                .any(|token| {
                    (token.starts_with("aigw_") || token.starts_with("sk-")) && token.len() >= 20
                })
        {
            return Err(AdminError::InvalidRequest);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    context: &AdminContext,
    incident_id: Uuid,
    event_type: &str,
    occurred_at: DateTime<Utc>,
    message: Option<&str>,
    before: Option<&Value>,
    after: Option<&Value>,
    gateway_request_id: Option<Uuid>,
    deployment_version: Option<&str>,
) -> Result<Value, AdminError> {
    Ok(sqlx::query_scalar::<_, Value>(
        r#"INSERT INTO incident_events (id, tenant_id, incident_id, actor_admin_id, event_type, occurred_at, message, before_state, after_state, gateway_request_id, deployment_version)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING to_jsonb(incident_events)"#,
    )
    .bind(Uuid::new_v4()).bind(context.tenant_id).bind(incident_id).bind(context.account_id)
    .bind(event_type).bind(occurred_at).bind(message).bind(before).bind(after)
    .bind(gateway_request_id).bind(deployment_version).fetch_one(&mut **transaction).await?)
}

#[cfg(test)]
mod tests {
    use super::validate_texts;

    #[test]
    fn incident_text_rejects_secrets_and_content_payloads_without_blocking_plain_language() {
        let safe = "Risk-based mitigation without request content".to_owned();
        assert!(validate_texts([Some(&safe)]).is_ok());

        for unsafe_text in [
            "Authorization: Bearer hidden",
            "access_token=hidden",
            "Prompt: full content",
            "{\"prompt\":\"full content\"}",
            "sk-12345678901234567890",
            "aigw_prod_12345678901234567890",
        ] {
            let value = unsafe_text.to_owned();
            assert!(
                validate_texts([Some(&value)]).is_err(),
                "unsafe incident text was accepted: {unsafe_text}"
            );
        }
    }
}
