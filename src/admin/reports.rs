use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::Row as _;
use uuid::Uuid;

use super::{AdminContext, AdminError, AdminService};

const MAX_REPORT_WINDOW: Duration = Duration::days(366);

#[derive(Clone, Deserialize)]
pub struct ReportWindowQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct ReportListQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub limit: Option<u16>,
    pub cursor: Option<String>,
}

#[derive(Deserialize)]
pub struct UsageQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub dimension: String,
}

#[derive(Serialize)]
pub struct CursorPage {
    pub items: Vec<Value>,
    pub next_cursor: Option<String>,
}

impl AdminService {
    pub async fn task_classification_shadow_report(
        &self,
        context: &AdminContext,
        query: ReportWindowQuery,
    ) -> Result<Value, AdminError> {
        validate_window(query.from, query.to)?;
        let report = sqlx::query_scalar(
            r#"
            WITH shadow_requests AS (
                SELECT *, policy_decision->'routing'->'task_classification' AS routing
                FROM gateway_requests
                WHERE tenant_id = $1 AND created_at >= $2 AND created_at < $3
                  AND policy_decision->'routing'->'task_classification'
                      ->'classification'->>'mode' = 'shadow'
            ), ranked_usage AS (
                SELECT ur.*, row_number() OVER (
                    PARTITION BY ur.tenant_id, ur.provider_attempt_id
                    ORDER BY CASE ur.source WHEN 'provider_reported' THEN 0 ELSE 1 END
                ) AS preference
                FROM usage_records ur
                JOIN shadow_requests sr
                  ON (sr.tenant_id, sr.id) = (ur.tenant_id, ur.gateway_request_id)
            ), stats AS (
                SELECT count(*)::bigint AS requests,
                       count(*) FILTER (WHERE status = 'completed')::bigint AS successful,
                       count(*) FILTER (WHERE (routing->>'jev_attempted')::boolean)::bigint AS jev_attempted,
                       count(*) FILTER (WHERE routing->>'jev_opinion' IS NOT NULL)::bigint AS jev_opinions,
                       count(*) FILTER (
                           WHERE routing->>'jev_opinion' IS NOT NULL
                             AND (routing->'classification'->'shortlist')
                                 ? (routing->>'jev_opinion')
                       )::bigint AS jev_agreements,
                       count(*) FILTER (WHERE (routing->>'changed')::boolean)::bigint AS route_changes,
                       percentile_cont(0.95) WITHIN GROUP (
                           ORDER BY extract(epoch FROM total_latency) * 1000
                       ) FILTER (WHERE total_latency IS NOT NULL) AS p95_latency_ms,
                       percentile_cont(0.95) WITHIN GROUP (
                           ORDER BY (routing->'classification'->>'elapsed_us')::double precision
                       ) FILTER (WHERE routing->'classification'->>'elapsed_us' IS NOT NULL)
                           AS classifier_p95_us,
                       percentile_cont(0.95) WITHIN GROUP (
                           ORDER BY (routing->>'jev_elapsed_ms')::double precision
                       ) FILTER (WHERE routing->>'jev_elapsed_ms' IS NOT NULL) AS jev_p95_ms
                FROM shadow_requests
            ), usage_stats AS (
                SELECT sum(cost_microusd)::bigint AS cost_microusd
                FROM ranked_usage WHERE preference = 1
            )
            SELECT jsonb_build_object(
                'from', $2, 'to', $3,
                'requests', s.requests,
                'successful_requests', s.successful,
                'success_rate', CASE WHEN s.requests = 0 THEN NULL
                    ELSE s.successful::double precision / s.requests END,
                'jev_attempted', s.jev_attempted,
                'jev_opinions', s.jev_opinions,
                'jev_agreements', s.jev_agreements,
                'jev_agreement_rate', CASE WHEN s.jev_opinions = 0 THEN NULL
                    ELSE s.jev_agreements::double precision / s.jev_opinions END,
                'route_changes', s.route_changes,
                'p95_latency_ms', s.p95_latency_ms,
                'classifier_p95_us', s.classifier_p95_us,
                'jev_p95_ms', s.jev_p95_ms,
                'observed_inference_cost_microusd', u.cost_microusd,
                'routing_cost_microusd', NULL,
                'routing_cost_unavailable_reason',
                    'Jev does not return billable usage; classifier runs locally'
            ) FROM stats s CROSS JOIN usage_stats u
            "#,
        )
        .bind(context.tenant_id)
        .bind(query.from)
        .bind(query.to)
        .fetch_one(&self.pool)
        .await?;
        Ok(report)
    }

    pub async fn overview_report(
        &self,
        context: &AdminContext,
        query: ReportWindowQuery,
    ) -> Result<Value, AdminError> {
        validate_window(query.from, query.to)?;
        let report = sqlx::query_scalar(
            r#"
            WITH filtered_requests AS (
                SELECT * FROM gateway_requests
                WHERE tenant_id = $1 AND created_at >= $2 AND created_at < $3
            ), ranked_usage AS (
                SELECT ur.*, row_number() OVER (
                    PARTITION BY ur.tenant_id, ur.provider_attempt_id
                    ORDER BY CASE ur.source WHEN 'provider_reported' THEN 0 ELSE 1 END
                ) AS preference
                FROM usage_records ur
                JOIN filtered_requests gr
                  ON (gr.tenant_id, gr.id) = (ur.tenant_id, ur.gateway_request_id)
            ), canonical_usage AS (
                SELECT * FROM ranked_usage WHERE preference = 1
            ), request_stats AS (
                SELECT count(*)::bigint AS requests,
                       count(*) FILTER (WHERE status = 'completed')::bigint AS successful,
                       count(*) FILTER (WHERE status <> 'completed')::bigint AS unsuccessful,
                       percentile_cont(0.95) WITHIN GROUP (
                           ORDER BY extract(epoch FROM total_latency) * 1000
                       ) FILTER (WHERE total_latency IS NOT NULL) AS p95_latency_ms,
                       percentile_cont(0.95) WITHIN GROUP (
                           ORDER BY extract(epoch FROM gateway_overhead) * 1000
                       ) FILTER (WHERE gateway_overhead IS NOT NULL) AS p95_gateway_overhead_ms,
                       percentile_cont(0.50) WITHIN GROUP (ORDER BY extract(epoch FROM queue_duration) * 1000)
                         FILTER (WHERE queue_duration IS NOT NULL) AS queue_p50_ms,
                       percentile_cont(0.95) WITHIN GROUP (ORDER BY extract(epoch FROM queue_duration) * 1000)
                         FILTER (WHERE queue_duration IS NOT NULL) AS queue_p95_ms,
                       count(*) FILTER (WHERE status = 'rejected' AND queue_entered_at IS NOT NULL)::bigint AS queue_rejected,
                       count(*) FILTER (WHERE status = 'rejected' AND queue_left_at >= deadline_at)::bigint AS queue_expired
                FROM filtered_requests
            ), usage_stats AS (
                SELECT COALESCE(sum(input_tokens), 0)::bigint AS input_tokens,
                       COALESCE(sum(output_tokens), 0)::bigint AS output_tokens,
                       COALESCE(sum(cached_input_tokens), 0)::bigint AS cached_input_tokens,
                       COALESCE(sum(cache_write_input_tokens), 0)::bigint AS cache_write_input_tokens,
                       sum(cost_microusd)::bigint AS cost_microusd,
                       count(*) FILTER (WHERE source = 'estimated')::bigint AS unreconciled
                FROM canonical_usage
            ), attempt_stats AS (
                SELECT percentile_cont(0.95) WITHIN GROUP (
                           ORDER BY extract(epoch FROM pa.provider_ttft) * 1000
                       ) FILTER (WHERE pa.provider_ttft IS NOT NULL) AS p95_ttft_ms
                FROM provider_attempts pa
                JOIN filtered_requests gr
                  ON (gr.tenant_id, gr.id) = (pa.tenant_id, pa.gateway_request_id)
            )
            SELECT jsonb_build_object(
                'from', $2, 'to', $3,
                'requests', rs.requests,
                'successful_requests', rs.successful,
                'unsuccessful_requests', rs.unsuccessful,
                'success_rate', CASE WHEN rs.requests = 0 THEN NULL
                    ELSE rs.successful::double precision / rs.requests::double precision END,
                'input_tokens', us.input_tokens,
                'output_tokens', us.output_tokens,
                'total_tokens', us.input_tokens + us.output_tokens,
                'cached_input_tokens', us.cached_input_tokens,
                'cache_write_input_tokens', us.cache_write_input_tokens,
                'p95_latency_ms', rs.p95_latency_ms,
                'p95_gateway_overhead_ms', rs.p95_gateway_overhead_ms,
                'p95_ttft_ms', ats.p95_ttft_ms,
                'cost_microusd', us.cost_microusd,
                'unreconciled_attempts', us.unreconciled,
                'cost_unavailable_reason', CASE WHEN us.cost_microusd IS NULL
                    THEN 'cost attribution is not populated until model pricing is active' END,
                'queue', jsonb_build_object(
                    'available', true,
                    'p50_ms', rs.queue_p50_ms,
                    'p95_ms', rs.queue_p95_ms,
                    'rejected', rs.queue_rejected,
                    'expired', rs.queue_expired
                )
            )
            FROM request_stats rs CROSS JOIN usage_stats us CROSS JOIN attempt_stats ats
            "#,
        )
        .bind(context.tenant_id)
        .bind(query.from)
        .bind(query.to)
        .fetch_one(&self.pool)
        .await?;
        let mut report: Value = report;
        if let Some(scheduler) = &self.scheduler {
            report["queue"]["runtime"] = scheduler
                .snapshot(context.tenant_id)
                .map_err(|_| AdminError::Unavailable)?;
        } else {
            report["queue"] = json!({
                "available": false,
                "unavailable_reason": "scheduler is not attached"
            });
        }
        Ok(report)
    }

    pub async fn identity_map(&self, context: &AdminContext) -> Result<Value, AdminError> {
        let report = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'paths', COALESCE((
                    SELECT jsonb_agg(jsonb_build_object(
                        'credential', jsonb_build_object(
                            'id', c.id, 'display_name', c.display_name,
                            'key_prefix', c.key_prefix, 'status', c.status,
                            'last_used_at', c.last_used_at, 'version', c.version
                        ),
                        'user', CASE WHEN u.id IS NULL THEN NULL ELSE jsonb_build_object(
                            'id', u.id, 'display_name', u.display_name, 'status', u.status) END,
                        'device', CASE WHEN d.id IS NULL THEN NULL ELSE jsonb_build_object(
                            'id', d.id, 'display_name', d.display_name, 'status', d.status) END,
                        'client', CASE WHEN cl.id IS NULL THEN NULL ELSE jsonb_build_object(
                            'id', cl.id, 'display_name', cl.display_name, 'status', cl.status) END,
                        'agent', CASE WHEN a.id IS NULL THEN NULL ELSE jsonb_build_object(
                            'id', a.id, 'display_name', a.display_name, 'status', a.status) END
                    ) ORDER BY c.created_at DESC, c.id DESC)
                    FROM credentials c
                    LEFT JOIN users u ON (u.tenant_id, u.id) = (c.tenant_id, c.user_id)
                    LEFT JOIN devices d ON (d.tenant_id, d.id) = (c.tenant_id, c.device_id)
                    LEFT JOIN clients cl ON (cl.tenant_id, cl.id) = (c.tenant_id, c.client_id)
                    LEFT JOIN agents a ON (a.tenant_id, a.id) = (c.tenant_id, c.agent_id)
                    WHERE c.tenant_id = $1
                ), '[]'::jsonb),
                'unassociated', jsonb_build_object(
                    'users', COALESCE((SELECT jsonb_agg(to_jsonb(u) ORDER BY u.created_at, u.id)
                        FROM users u WHERE u.tenant_id = $1 AND NOT EXISTS (
                            SELECT 1 FROM credentials c
                            WHERE (c.tenant_id, c.user_id) = (u.tenant_id, u.id))), '[]'::jsonb),
                    'devices', COALESCE((SELECT jsonb_agg(to_jsonb(d) ORDER BY d.created_at, d.id)
                        FROM devices d WHERE d.tenant_id = $1 AND NOT EXISTS (
                            SELECT 1 FROM credentials c
                            WHERE (c.tenant_id, c.device_id) = (d.tenant_id, d.id))), '[]'::jsonb),
                    'clients', COALESCE((SELECT jsonb_agg(to_jsonb(cl) ORDER BY cl.created_at, cl.id)
                        FROM clients cl WHERE cl.tenant_id = $1 AND NOT EXISTS (
                            SELECT 1 FROM credentials c
                            WHERE (c.tenant_id, c.client_id) = (cl.tenant_id, cl.id))), '[]'::jsonb),
                    'agents', COALESCE((SELECT jsonb_agg(to_jsonb(a) ORDER BY a.created_at, a.id)
                        FROM agents a WHERE a.tenant_id = $1 AND NOT EXISTS (
                            SELECT 1 FROM credentials c
                            WHERE (c.tenant_id, c.agent_id) = (a.tenant_id, a.id))), '[]'::jsonb)
                )
            )
            "#,
        )
        .bind(context.tenant_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(report)
    }

    pub async fn request_page(
        &self,
        context: &AdminContext,
        query: ReportListQuery,
    ) -> Result<CursorPage, AdminError> {
        validate_window(query.from, query.to)?;
        let limit = validate_limit(query.limit)?;
        let (cursor_time, cursor_id) = decode_cursor(query.cursor.as_deref(), query.to)?;
        let rows = sqlx::query(
            r#"
            WITH ranked_usage AS (
                SELECT ur.*, row_number() OVER (
                    PARTITION BY ur.tenant_id, ur.provider_attempt_id
                    ORDER BY CASE ur.source WHEN 'provider_reported' THEN 0 ELSE 1 END
                ) AS preference
                FROM usage_records ur
                WHERE ur.tenant_id = $1
            ), usage_by_request AS (
                SELECT gateway_request_id,
                       sum(input_tokens)::bigint AS input_tokens,
                       sum(output_tokens)::bigint AS output_tokens,
                       sum(cost_microusd)::bigint AS cost_microusd,
                       count(*) FILTER (WHERE source = 'estimated')::bigint AS unreconciled
                FROM ranked_usage WHERE preference = 1 GROUP BY gateway_request_id
            )
            SELECT gr.created_at, gr.id, jsonb_build_object(
                'id', gr.id, 'request_id', gr.request_id, 'trace_id', gr.trace_id,
                'status', gr.status, 'operation', gr.operation,
                'requested_model', gr.requested_model, 'selected_model', gr.selected_model,
                'selected_provider', gr.selected_provider, 'workload_class', gr.workload_class,
                'routing_reason', COALESCE(gr.policy_decision->'routing'->>'reason', 'registry'),
                'created_at', gr.created_at, 'completed_at', gr.completed_at,
                'total_latency_ms', extract(epoch FROM gr.total_latency) * 1000,
                'principal', jsonb_build_object(
                    'user', u.display_name, 'device', d.display_name,
                    'client', cl.display_name, 'agent', a.display_name,
                    'credential', c.display_name
                ),
                'usage', jsonb_build_object(
                    'input_tokens', COALESCE(ubr.input_tokens, 0),
                    'output_tokens', COALESCE(ubr.output_tokens, 0),
                    'cost_microusd', ubr.cost_microusd,
                    'unreconciled_attempts', COALESCE(ubr.unreconciled, 0)
                )
            ) AS item
            FROM gateway_requests gr
            LEFT JOIN usage_by_request ubr ON ubr.gateway_request_id = gr.id
            LEFT JOIN users u ON (u.tenant_id, u.id) = (gr.tenant_id, gr.user_id)
            LEFT JOIN devices d ON (d.tenant_id, d.id) = (gr.tenant_id, gr.device_id)
            LEFT JOIN clients cl ON (cl.tenant_id, cl.id) = (gr.tenant_id, gr.client_id)
            LEFT JOIN agents a ON (a.tenant_id, a.id) = (gr.tenant_id, gr.agent_id)
            LEFT JOIN credentials c ON (c.tenant_id, c.id) = (gr.tenant_id, gr.credential_id)
            WHERE gr.tenant_id = $1 AND gr.created_at >= $2 AND gr.created_at < $3
              AND (gr.created_at, gr.id) < ($4, $5)
            ORDER BY gr.created_at DESC, gr.id DESC LIMIT $6
            "#,
        )
        .bind(context.tenant_id)
        .bind(query.from)
        .bind(query.to)
        .bind(cursor_time)
        .bind(cursor_id)
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await?;
        page_from_rows(rows, limit, "created_at", "id", "item")
    }

    pub async fn request_report(
        &self,
        context: &AdminContext,
        id: Uuid,
    ) -> Result<Value, AdminError> {
        let report = sqlx::query_scalar(
            r#"
            WITH ranked_usage AS (
                SELECT ur.*, row_number() OVER (
                    PARTITION BY ur.tenant_id, ur.provider_attempt_id
                    ORDER BY CASE ur.source WHEN 'provider_reported' THEN 0 ELSE 1 END
                ) AS preference
                FROM usage_records ur
                WHERE ur.tenant_id = $1 AND ur.gateway_request_id = $2
            )
            SELECT jsonb_build_object(
                'request', to_jsonb(gr) - 'tenant_id',
                'parameters', jsonb_build_object(
                    'requested', (SELECT rps.parameters FROM request_parameter_snapshots rps
                        WHERE rps.tenant_id = $1 AND rps.gateway_request_id = $2
                          AND rps.stage = 'requested'),
                    'provider', (SELECT rps.parameters FROM request_parameter_snapshots rps
                        WHERE rps.tenant_id = $1 AND rps.gateway_request_id = $2
                          AND rps.stage = 'provider')
                ),
                'principal', jsonb_build_object(
                    'user', CASE WHEN u.id IS NULL THEN NULL ELSE jsonb_build_object('id', u.id, 'name', u.display_name) END,
                    'device', CASE WHEN d.id IS NULL THEN NULL ELSE jsonb_build_object('id', d.id, 'name', d.display_name) END,
                    'client', CASE WHEN cl.id IS NULL THEN NULL ELSE jsonb_build_object('id', cl.id, 'name', cl.display_name) END,
                    'agent', CASE WHEN a.id IS NULL THEN NULL ELSE jsonb_build_object('id', a.id, 'name', a.display_name) END,
                    'credential', CASE WHEN c.id IS NULL THEN NULL ELSE jsonb_build_object(
                        'id', c.id, 'name', c.display_name, 'key_prefix', c.key_prefix) END
                ),
                'attempts', COALESCE((SELECT jsonb_agg(to_jsonb(pa) - 'tenant_id' ORDER BY pa.attempt_number)
                    FROM provider_attempts pa
                    WHERE pa.tenant_id = $1 AND pa.gateway_request_id = $2), '[]'::jsonb),
                'usage', COALESCE((SELECT jsonb_agg(
                    to_jsonb(ru) - ARRAY['tenant_id', 'preference']::text[] ORDER BY ru.recorded_at, ru.id)
                    FROM ranked_usage ru WHERE ru.preference = 1), '[]'::jsonb),
                'retry_fallback_history', jsonb_build_object(
                    'available', true,
                    'same_request_retry', false,
                    'policy', 'one logical request makes one provider call',
                    'events', COALESCE((SELECT jsonb_agg(jsonb_build_object(
                        'attempt_number', pa.attempt_number,
                        'attempt_kind', 'initial',
                        'provider', pa.provider,
                        'model', pa.model,
                        'status', pa.status,
                        'error_type', pa.error_type,
                        'routing_reason', COALESCE(
                            gr.policy_decision->'routing'->>'reason', 'registry')
                    ) ORDER BY pa.attempt_number)
                    FROM provider_attempts pa
                    WHERE pa.tenant_id = $1 AND pa.gateway_request_id = $2), '[]'::jsonb)
                ),
                'trace_spans', jsonb_build_object(
                    'available', false,
                    'trace_id', gr.trace_id,
                    'unavailable_reason', 'spans are exported over OTLP and are not stored by the gateway'
                )
            )
            FROM gateway_requests gr
            LEFT JOIN users u ON (u.tenant_id, u.id) = (gr.tenant_id, gr.user_id)
            LEFT JOIN devices d ON (d.tenant_id, d.id) = (gr.tenant_id, gr.device_id)
            LEFT JOIN clients cl ON (cl.tenant_id, cl.id) = (gr.tenant_id, gr.client_id)
            LEFT JOIN agents a ON (a.tenant_id, a.id) = (gr.tenant_id, gr.agent_id)
            LEFT JOIN credentials c ON (c.tenant_id, c.id) = (gr.tenant_id, gr.credential_id)
            WHERE gr.tenant_id = $1 AND gr.id = $2
            "#,
        )
        .bind(context.tenant_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AdminError::NotFound)?;
        Ok(report)
    }

    pub async fn usage_report(
        &self,
        context: &AdminContext,
        query: UsageQuery,
    ) -> Result<Value, AdminError> {
        validate_window(query.from, query.to)?;
        if !matches!(
            query.dimension.as_str(),
            "user"
                | "device"
                | "client"
                | "agent"
                | "credential"
                | "model"
                | "provider"
                | "workload"
        ) {
            return Err(AdminError::InvalidRequest);
        }
        let report = sqlx::query_scalar(
            r#"
            WITH ranked_usage AS (
                SELECT ur.*, row_number() OVER (
                    PARTITION BY ur.tenant_id, ur.provider_attempt_id
                    ORDER BY CASE ur.source WHEN 'provider_reported' THEN 0 ELSE 1 END
                ) AS preference
                FROM usage_records ur
                WHERE ur.tenant_id = $1
            ), canonical AS (
                SELECT ru.*, gr.user_id, gr.device_id, gr.client_id, gr.agent_id,
                       gr.credential_id, gr.workload_class, pa.model, pa.provider
                FROM ranked_usage ru
                JOIN gateway_requests gr
                  ON (gr.tenant_id, gr.id) = (ru.tenant_id, ru.gateway_request_id)
                JOIN provider_attempts pa
                  ON (pa.tenant_id, pa.gateway_request_id, pa.id) =
                     (ru.tenant_id, ru.gateway_request_id, ru.provider_attempt_id)
                WHERE ru.preference = 1 AND gr.created_at >= $2 AND gr.created_at < $3
            ), labeled AS (
                SELECT canonical.*,
                    CASE $4
                        WHEN 'user' THEN canonical.user_id::text
                        WHEN 'device' THEN canonical.device_id::text
                        WHEN 'client' THEN canonical.client_id::text
                        WHEN 'agent' THEN canonical.agent_id::text
                        WHEN 'credential' THEN canonical.credential_id::text
                        WHEN 'model' THEN canonical.model
                        WHEN 'provider' THEN canonical.provider
                        WHEN 'workload' THEN canonical.workload_class END AS dimension_id,
                    CASE $4
                        WHEN 'user' THEN u.display_name WHEN 'device' THEN d.display_name
                        WHEN 'client' THEN cl.display_name WHEN 'agent' THEN a.display_name
                        WHEN 'credential' THEN cr.display_name WHEN 'model' THEN canonical.model
                        WHEN 'provider' THEN canonical.provider
                        WHEN 'workload' THEN canonical.workload_class END AS dimension_name
                FROM canonical
                LEFT JOIN users u ON (u.tenant_id, u.id) = (canonical.tenant_id, canonical.user_id)
                LEFT JOIN devices d ON (d.tenant_id, d.id) = (canonical.tenant_id, canonical.device_id)
                LEFT JOIN clients cl ON (cl.tenant_id, cl.id) = (canonical.tenant_id, canonical.client_id)
                LEFT JOIN agents a ON (a.tenant_id, a.id) = (canonical.tenant_id, canonical.agent_id)
                LEFT JOIN credentials cr ON (cr.tenant_id, cr.id) = (canonical.tenant_id, canonical.credential_id)
            ), grouped AS (
                SELECT COALESCE(dimension_id, 'unassigned') AS dimension_id,
                       COALESCE(dimension_name, 'Unassigned') AS dimension_name,
                       sum(input_tokens)::bigint AS input_tokens,
                       sum(output_tokens)::bigint AS output_tokens,
                       sum(cached_input_tokens)::bigint AS cached_input_tokens,
                       sum(cache_write_input_tokens)::bigint AS cache_write_input_tokens,
                       sum(reasoning_tokens)::bigint AS reasoning_tokens,
                       sum(cost_microusd)::bigint AS cost_microusd,
                       count(*)::bigint AS attempts,
                       count(*) FILTER (WHERE source = 'estimated')::bigint AS unreconciled_attempts
                FROM labeled GROUP BY dimension_id, dimension_name
            )
            SELECT jsonb_build_object(
                'from', $2, 'to', $3, 'dimension', $4,
                'groups', COALESCE((SELECT jsonb_agg(to_jsonb(grouped)
                    ORDER BY (input_tokens + output_tokens) DESC, dimension_id) FROM grouped), '[]'::jsonb),
                'cost_available', EXISTS (SELECT 1 FROM grouped WHERE cost_microusd IS NOT NULL),
                'cost_unavailable_reason', CASE WHEN NOT EXISTS (
                    SELECT 1 FROM grouped WHERE cost_microusd IS NOT NULL
                ) THEN 'cost attribution is not populated until model pricing is active' END,
                'team', jsonb_build_object(
                    'available', false,
                    'unavailable_reason', 'team membership is not part of the current Principal or ledger schema'
                )
            )
            "#,
        )
        .bind(context.tenant_id)
        .bind(query.from)
        .bind(query.to)
        .bind(query.dimension)
        .fetch_one(&self.pool)
        .await?;
        Ok(report)
    }

    pub async fn audit_page(
        &self,
        context: &AdminContext,
        query: ReportListQuery,
    ) -> Result<CursorPage, AdminError> {
        validate_window(query.from, query.to)?;
        let limit = validate_limit(query.limit)?;
        let (cursor_time, cursor_id) = decode_cursor(query.cursor.as_deref(), query.to)?;
        let rows = sqlx::query(
            r#"
            SELECT al.occurred_at, al.id, al.action, al.target_type, al.target_id,
                   al.actor_role, al.source_ip::text AS source_ip, al.peer_ip::text AS peer_ip,
                   al.request_id, al.before_state, al.after_state,
                   aa.display_name AS actor_name
            FROM audit_logs al
            LEFT JOIN admin_accounts aa
              ON (aa.tenant_id, aa.id) = (al.tenant_id, al.actor_admin_id)
            WHERE al.tenant_id = $1 AND al.occurred_at >= $2 AND al.occurred_at < $3
              AND (al.occurred_at, al.id) < ($4, $5)
            ORDER BY al.occurred_at DESC, al.id DESC LIMIT $6
            "#,
        )
        .bind(context.tenant_id)
        .bind(query.from)
        .bind(query.to)
        .bind(cursor_time)
        .bind(cursor_id)
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await?;

        let has_more = rows.len() > usize::from(limit);
        let mut items = Vec::with_capacity(usize::from(limit));
        let mut last = None;
        for row in rows.into_iter().take(usize::from(limit)) {
            let target_type: String = row.try_get("target_type")?;
            let before: Option<Value> = row.try_get("before_state")?;
            let after: Option<Value> = row.try_get("after_state")?;
            let occurred_at: DateTime<Utc> = row.try_get("occurred_at")?;
            let id: Uuid = row.try_get("id")?;
            last = Some((occurred_at, id));
            items.push(json!({
                "id": id,
                "occurred_at": occurred_at,
                "actor_name": row.try_get::<Option<String>, _>("actor_name")?,
                "actor_role": row.try_get::<Option<String>, _>("actor_role")?,
                "action": row.try_get::<String, _>("action")?,
                "target_type": target_type,
                "target_id": row.try_get::<Option<Uuid>, _>("target_id")?,
                "before": safe_audit_state(&target_type, before),
                "after": safe_audit_state(&target_type, after),
                "source_ip": row.try_get::<Option<String>, _>("source_ip")?,
                "peer_ip": row.try_get::<Option<String>, _>("peer_ip")?,
                "request_id": row.try_get::<Option<String>, _>("request_id")?,
            }));
        }
        Ok(CursorPage {
            items,
            next_cursor: has_more
                .then(|| last.map(|value| encode_cursor(value.0, value.1)))
                .flatten(),
        })
    }

    pub async fn catalog_report(&self, context: &AdminContext) -> Result<Value, AdminError> {
        let report = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'providers', COALESCE((SELECT jsonb_agg(
                    (to_jsonb(p) - ARRAY['tenant_id', 'credential_ref']::text[]) ||
                    jsonb_build_object('credential_configured', btrim(p.credential_ref) <> '')
                    ORDER BY p.provider_key
                ) FROM providers p WHERE p.tenant_id = $1), '[]'::jsonb),
                'models', COALESCE((SELECT jsonb_agg(
                    (to_jsonb(m) - 'tenant_id') || jsonb_build_object(
                        'provider_key', p.provider_key,
                        'observed_requests', metrics.requests,
                        'observed_success_rate', metrics.success_rate,
                        'observed_avg_latency_ms', metrics.avg_latency_ms
                    )
                    ORDER BY m.public_model_id
                ) FROM models m JOIN providers p
                    ON (p.tenant_id, p.id) = (m.tenant_id, m.provider_id)
                  LEFT JOIN (
                    SELECT r.tenant_id, r.selected_model_id,
                           count(*)::bigint AS requests,
                           count(*) FILTER (WHERE r.status = 'completed')::double precision
                             / NULLIF(count(*), 0) AS success_rate,
                           avg(extract(epoch FROM r.total_latency) * 1000)
                             FILTER (WHERE r.total_latency IS NOT NULL) AS avg_latency_ms
                    FROM gateway_requests r
                    WHERE r.tenant_id = $1
                      AND r.created_at >= now() - INTERVAL '30 days'
                    GROUP BY r.tenant_id, r.selected_model_id
                  ) metrics ON (metrics.tenant_id, metrics.selected_model_id) = (m.tenant_id, m.id)
                  WHERE m.tenant_id = $1), '[]'::jsonb),
                'aliases', COALESCE((SELECT jsonb_agg(
                    (to_jsonb(a) - 'tenant_id') || jsonb_build_object(
                        'public_model_id', m.public_model_id,
                        'provider_model_id', m.provider_model_id,
                        'provider_key', p.provider_key
                    ) ORDER BY a.operation, a.quality_tier
                ) FROM model_aliases a
                  JOIN models m ON (m.tenant_id, m.id) = (a.tenant_id, a.model_id)
                  JOIN providers p ON (p.tenant_id, p.id) = (m.tenant_id, m.provider_id)
                  WHERE a.tenant_id = $1), '[]'::jsonb),
                'activation', jsonb_build_object(
                    'available', true
                )
            )
            "#,
        )
        .bind(context.tenant_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(report)
    }

    pub async fn governance_report(&self, context: &AdminContext) -> Result<Value, AdminError> {
        let report = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'policies', COALESCE((SELECT jsonb_agg(to_jsonb(p) - 'tenant_id'
                    ORDER BY p.policy_key, p.revision DESC)
                    FROM policies p WHERE p.tenant_id = $1), '[]'::jsonb),
                'quotas', COALESCE((SELECT jsonb_agg(to_jsonb(q) - 'tenant_id'
                    ORDER BY q.quota_key, q.revision DESC)
                    FROM quotas q WHERE q.tenant_id = $1), '[]'::jsonb),
                'policy_runtime', jsonb_build_object(
                    'available', true,
                    'active_count', (SELECT count(*) FROM policies p
                        WHERE p.tenant_id = $1 AND p.lifecycle_state = 'active'),
                    'resolution_order', jsonb_build_array(
                        'global', 'user', 'device', 'client', 'agent', 'credential', 'request_hint'
                    )
                ),
                'quota_runtime', jsonb_build_object(
                    'available', true,
                    'active_count', (SELECT count(*) FROM quotas q
                        WHERE q.tenant_id = $1 AND q.lifecycle_state = 'active'),
                    'window_count', (SELECT count(*) FROM quota_windows w
                        WHERE w.tenant_id = $1 AND w.window_end > now()),
                    'timezone', 'UTC',
                    'windows', COALESCE((SELECT jsonb_agg(window_item ORDER BY reset_at, quota_key, limit_kind)
                        FROM (
                            SELECT q.quota_key, q.revision, w.limit_kind,
                                   CASE w.limit_kind
                                     WHEN 'requests_per_minute' THEN w.reserved_requests
                                     WHEN 'cost_per_day_microusd' THEN w.reserved_cost_microusd
                                     WHEN 'cost_per_month_microusd' THEN w.reserved_cost_microusd
                                     ELSE w.reserved_tokens
                                   END AS current,
                                   (q.configuration->'limits'->>w.limit_kind)::bigint AS limit,
                                   w.window_end AS reset_at,
                                   jsonb_build_object(
                                     'quota_key', q.quota_key,
                                     'revision', q.revision,
                                     'limit_kind', w.limit_kind,
                                     'current', CASE w.limit_kind
                                       WHEN 'requests_per_minute' THEN w.reserved_requests
                                       WHEN 'cost_per_day_microusd' THEN w.reserved_cost_microusd
                                       WHEN 'cost_per_month_microusd' THEN w.reserved_cost_microusd
                                       ELSE w.reserved_tokens
                                     END,
                                     'limit', (q.configuration->'limits'->>w.limit_kind)::bigint,
                                     'reset_at', w.window_end
                                   ) AS window_item
                            FROM quota_windows w
                            JOIN quotas q ON (q.tenant_id, q.id) = (w.tenant_id, w.quota_id)
                            WHERE w.tenant_id = $1 AND w.window_end > now()
                            ORDER BY w.window_end, q.quota_key, w.limit_kind
                            LIMIT 100
                        ) current_windows), '[]'::jsonb)
                ),
                'scheduler', '{}'::jsonb
            )
            "#,
        )
        .bind(context.tenant_id)
        .fetch_one(&self.pool)
        .await?;
        let mut report: Value = report;
        report["scheduler"] = match &self.scheduler {
            Some(scheduler) => scheduler
                .snapshot(context.tenant_id)
                .map_err(|_| AdminError::Unavailable)?,
            None => json!({"available": false, "unavailable_reason": "scheduler is not attached"}),
        };
        Ok(report)
    }

    pub async fn system_health_report(
        &self,
        context: &AdminContext,
        telemetry_configured: bool,
    ) -> Result<Value, AdminError> {
        let database_time: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
            .fetch_one(&self.pool)
            .await?;
        let scheduler = match &self.scheduler {
            Some(scheduler) => scheduler
                .snapshot(context.tenant_id)
                .map_err(|_| AdminError::Unavailable)?,
            None => json!({"available": false, "unavailable_reason": "scheduler is not attached"}),
        };
        Ok(json!({
            "status": "ok",
            "tenant_id": context.tenant_id,
            "checked_at": database_time,
            "components": {
                "gateway": {"status": "ok"},
                "admin_database": {"status": "ok"},
                "telemetry_export": {
                    "status": if telemetry_configured { "configured" } else { "not_configured" },
                    "delivery_verified": false
                },
                "scheduler": {"status": "ok", "runtime": scheduler}
            }
        }))
    }
}

fn validate_window(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<(), AdminError> {
    let duration = to - from;
    if duration <= Duration::zero()
        || duration > MAX_REPORT_WINDOW
        || to > Utc::now() + Duration::minutes(5)
    {
        return Err(AdminError::InvalidRequest);
    }
    Ok(())
}

fn validate_limit(limit: Option<u16>) -> Result<u16, AdminError> {
    let limit = limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(AdminError::InvalidRequest);
    }
    Ok(limit)
}

fn decode_cursor(
    cursor: Option<&str>,
    default_time: DateTime<Utc>,
) -> Result<(DateTime<Utc>, Uuid), AdminError> {
    let Some(cursor) = cursor else {
        return Ok((default_time, Uuid::max()));
    };
    let (time, id) = cursor.split_once('|').ok_or(AdminError::InvalidRequest)?;
    let time = DateTime::parse_from_rfc3339(time)
        .map_err(|_| AdminError::InvalidRequest)?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(id).map_err(|_| AdminError::InvalidRequest)?;
    Ok((time, id))
}

fn encode_cursor(time: DateTime<Utc>, id: Uuid) -> String {
    format!("{}|{id}", time.to_rfc3339())
}

fn page_from_rows(
    rows: Vec<sqlx::postgres::PgRow>,
    limit: u16,
    time_column: &str,
    id_column: &str,
    value_column: &str,
) -> Result<CursorPage, AdminError> {
    let has_more = rows.len() > usize::from(limit);
    let mut items = Vec::with_capacity(usize::from(limit));
    let mut last = None;
    for row in rows.into_iter().take(usize::from(limit)) {
        let time: DateTime<Utc> = row.try_get(time_column)?;
        let id: Uuid = row.try_get(id_column)?;
        last = Some((time, id));
        items.push(row.try_get(value_column)?);
    }
    Ok(CursorPage {
        items,
        next_cursor: has_more
            .then(|| last.map(|value| encode_cursor(value.0, value.1)))
            .flatten(),
    })
}

fn safe_audit_state(target_type: &str, state: Option<Value>) -> Option<Value> {
    let allowed = match target_type {
        "user" | "device" | "client" | "agent" => &[
            "id",
            "display_name",
            "status",
            "version",
            "created_at",
            "updated_at",
        ][..],
        "credential" => &[
            "id",
            "display_name",
            "key_prefix",
            "user_id",
            "device_id",
            "client_id",
            "agent_id",
            "status",
            "expires_at",
            "last_used_at",
            "revoked_at",
            "version",
            "created_at",
            "updated_at",
            "rotated_from_id",
        ],
        "provider" => &[
            "id",
            "provider_key",
            "display_name",
            "provider_type",
            "base_url",
            "credential_ref",
            "supported_operations",
            "enabled",
            "lifecycle_state",
            "version",
            "created_at",
            "updated_at",
        ],
        "model" => &[
            "id",
            "provider_id",
            "public_model_id",
            "provider_model_id",
            "display_name",
            "description",
            "quality_tier",
            "context_window",
            "max_output_tokens",
            "supported_operations",
            "capabilities",
            "supports_streaming",
            "supports_tools",
            "supports_reasoning",
            "supports_images",
            "supports_structured_output",
            "cost_metadata",
            "latency_class",
            "enabled",
            "lifecycle_state",
            "version",
            "created_at",
            "updated_at",
        ],
        "model_alias" => &[
            "id",
            "operation",
            "quality_tier",
            "model_id",
            "lifecycle_state",
            "enabled",
            "version",
            "created_at",
            "updated_at",
        ],
        "policy" | "quota" => &[
            "id",
            "policy_key",
            "quota_key",
            "revision",
            "user_id",
            "device_id",
            "client_id",
            "agent_id",
            "credential_id",
            "model_id",
            "provider_id",
            "configuration",
            "lifecycle_state",
            "version",
            "created_at",
            "updated_at",
        ],
        "admin_account" => &[
            "id",
            "display_name",
            "role",
            "status",
            "version",
            "created_at",
            "updated_at",
        ],
        "admin_session" => &["id", "expires_at", "revoked_at", "created_at"],
        _ => &[],
    };
    let Value::Object(object) = state? else {
        return None;
    };
    let filtered: Map<String, Value> = object
        .into_iter()
        .filter(|(key, _)| allowed.contains(&key.as_str()))
        .collect();
    Some(Value::Object(filtered))
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use serde_json::json;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::{ReportListQuery, ReportWindowQuery, UsageQuery};
    use crate::{
        admin::{AdminContext, AdminError, AdminRole},
        identity::CredentialService,
    };

    fn context(tenant_id: Uuid) -> AdminContext {
        AdminContext {
            tenant_id,
            account_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            role: AdminRole::Viewer,
        }
    }

    fn service(pool: PgPool) -> crate::admin::AdminService {
        let hash_key = b"report-test-credential-hash-key-at-least-32".to_vec();
        let credentials = CredentialService::new(
            pool.clone(),
            "v1".to_owned(),
            hash_key.clone(),
            Vec::new(),
            "test".to_owned(),
            16,
            std::time::Duration::from_secs(60),
        );
        crate::admin::AdminService::new(
            pool,
            credentials,
            crate::admin::AdminServiceConfig {
                hash_key,
                hash_key_id: "v1".to_owned(),
                retiring_hash_keys: Vec::new(),
                bootstrap_token: None,
                bootstrap_tenant_id: None,
                session_ttl: std::time::Duration::from_secs(900),
            },
        )
    }

    #[sqlx::test]
    async fn reports_are_tenant_scoped_actual_first_bounded_and_redacted(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let other_tenant = Uuid::new_v4();
        let request_one = Uuid::from_u128(101);
        let request_two = Uuid::from_u128(102);
        let attempt_one = Uuid::from_u128(201);
        let attempt_two = Uuid::from_u128(202);
        sqlx::query(
            r#"
            WITH first_request AS (
                INSERT INTO gateway_requests (
                    id, tenant_id, request_id, requested_model, selected_model,
                    selected_provider, status, created_at, started_at, completed_at,
                    operation, request_received_at, total_latency, gateway_overhead,
                    policy_decision
                ) VALUES (
                    $1, $5, 'request-one', 'model-one', 'model-one', 'provider-one',
                    'completed', now() - interval '1 minute', now() - interval '1 minute',
                    now() - interval '59 seconds', 'responses', now() - interval '1 minute',
                    interval '1 second', interval '40 milliseconds',
                    jsonb_build_object('routing', jsonb_build_object(
                        'task_classification', jsonb_build_object(
                            'classification', jsonb_build_object(
                                'mode', 'shadow', 'elapsed_us', 25,
                                'shortlist', jsonb_build_array('model-one', 'model-three')
                            ),
                            'jev_attempted', true, 'jev_elapsed_ms', 12,
                            'jev_opinion', 'model-one', 'changed', false
                        )
                    ))
                ) RETURNING id
            ), second_request AS (
                INSERT INTO gateway_requests (
                    id, tenant_id, request_id, requested_model, selected_model,
                    selected_provider, status, created_at, started_at, completed_at,
                    operation, request_received_at, total_latency, gateway_overhead,
                    policy_decision
                ) VALUES (
                    $2, $5, 'request-two', 'model-two', 'model-two', 'provider-two',
                    'failed', now() - interval '1 minute', now() - interval '1 minute',
                    now() - interval '59 seconds', 'messages', now() - interval '1 minute',
                    interval '1 second', interval '20 milliseconds',
                    jsonb_build_object('routing', jsonb_build_object(
                        'task_classification', jsonb_build_object(
                            'classification', jsonb_build_object(
                                'mode', 'shadow', 'elapsed_us', 50,
                                'shortlist', jsonb_build_array('model-one')
                            ),
                            'jev_attempted', true, 'jev_elapsed_ms', 24,
                            'jev_opinion', 'model-two', 'changed', true
                        )
                    ))
                ) RETURNING id
            ), other_request AS (
                INSERT INTO gateway_requests (
                    id, tenant_id, request_id, status, created_at, operation, request_received_at
                ) VALUES ($6, $7, 'other-tenant', 'completed', now(), 'responses', now())
                RETURNING id
            ), first_attempt AS (
                INSERT INTO provider_attempts (
                    id, tenant_id, gateway_request_id, provider, model, attempt_number,
                    started_at, completed_at, status, provider_ttft
                ) VALUES ($3, $5, $1, 'provider-one', 'model-one', 1,
                    now() - interval '1 minute', now() - interval '59 seconds',
                    'completed', interval '100 milliseconds') RETURNING id
            ), second_attempt AS (
                INSERT INTO provider_attempts (
                    id, tenant_id, gateway_request_id, provider, model, attempt_number,
                    started_at, completed_at, status
                ) VALUES ($4, $5, $2, 'provider-two', 'model-two', 1,
                    now() - interval '1 minute', now() - interval '59 seconds', 'failed')
                RETURNING id
            ), inserted_usage AS (
                INSERT INTO usage_records (
                    id, tenant_id, gateway_request_id, provider_attempt_id, source,
                    input_tokens, output_tokens, recorded_at
                ) VALUES
                    ($8, $5, $1, $3, 'estimated', 100, 50, now() - interval '59 seconds'),
                    ($9, $5, $1, $3, 'provider_reported', 10, 5, now() + interval '10 minutes'),
                    ($10, $5, $2, $4, 'estimated', 7, 3, now() - interval '59 seconds')
            )
            INSERT INTO audit_logs (
                id, tenant_id, action, target_type, target_id, before_state, after_state, occurred_at
            ) VALUES ($11, $5, 'admin.credential.update', 'credential', $12,
                $13, $14, now() - interval '30 seconds')
            "#,
        )
        .bind(request_one)
        .bind(request_two)
        .bind(attempt_one)
        .bind(attempt_two)
        .bind(tenant_id)
        .bind(Uuid::from_u128(103))
        .bind(other_tenant)
        .bind(Uuid::from_u128(301))
        .bind(Uuid::from_u128(302))
        .bind(Uuid::from_u128(303))
        .bind(Uuid::from_u128(401))
        .bind(Uuid::from_u128(501))
        .bind(json!({"id": Uuid::from_u128(501), "key_prefix": "aigw_test", "secret_hash": "hidden", "fingerprint": "hidden", "display_name": "before"}))
        .bind(json!({"id": Uuid::from_u128(501), "key_prefix": "aigw_test", "secret_hash": "hidden", "fingerprint": "hidden", "display_name": "after"}))
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"
            WITH provider AS (
                INSERT INTO providers (
                    id, tenant_id, provider_key, display_name, provider_type,
                    base_url, credential_ref, supported_operations, lifecycle_state
                ) VALUES (
                    $1, $4, 'ollama', 'Ollama', 'local', 'http://127.0.0.1:11434',
                    'env://OLLAMA_KEY', ARRAY['anthropic_messages'], 'active'
                ) RETURNING id
            ), model AS (
                INSERT INTO models (
                    id, tenant_id, provider_id, public_model_id, provider_model_id,
                    display_name, supported_operations, lifecycle_state
                ) VALUES (
                    $2, $4, $1, 'local-code', 'qwen3-coder:30b', 'Local code',
                    ARRAY['anthropic_messages'], 'active'
                ) RETURNING id
            )
            INSERT INTO model_aliases (
                id, tenant_id, operation, quality_tier, model_id, lifecycle_state
            ) VALUES ($3, $4, 'anthropic_messages', 'premium', $2, 'active')
            "#,
        )
        .bind(Uuid::from_u128(601))
        .bind(Uuid::from_u128(602))
        .bind(Uuid::from_u128(603))
        .bind(tenant_id)
        .execute(&pool)
        .await
        .unwrap();

        let service = service(pool);
        let context = context(tenant_id);
        let from = Utc::now() - Duration::hours(1);
        let to = Utc::now() + Duration::minutes(1);
        let overview = service
            .overview_report(&context, ReportWindowQuery { from, to })
            .await
            .unwrap();
        assert_eq!(overview["requests"], 2);
        assert_eq!(overview["input_tokens"], 17);
        assert_eq!(overview["output_tokens"], 8);
        assert_eq!(overview["p95_gateway_overhead_ms"], 39.0);
        assert_eq!(overview["unreconciled_attempts"], 1);
        assert!(overview["cost_microusd"].is_null());
        assert_eq!(overview["queue"]["available"], false);

        let shadow = service
            .task_classification_shadow_report(&context, ReportWindowQuery { from, to })
            .await
            .unwrap();
        assert_eq!(shadow["requests"], 2);
        assert_eq!(shadow["successful_requests"], 1);
        assert_eq!(shadow["success_rate"], 0.5);
        assert_eq!(shadow["jev_opinions"], 2);
        assert_eq!(shadow["jev_agreements"], 1);
        assert_eq!(shadow["jev_agreement_rate"], 0.5);
        assert_eq!(shadow["route_changes"], 1);
        assert_eq!(shadow["classifier_p95_us"], 48.75);
        assert_eq!(shadow["jev_p95_ms"], 23.4);
        assert!(shadow["routing_cost_microusd"].is_null());

        let usage = service
            .usage_report(
                &context,
                UsageQuery {
                    from,
                    to,
                    dimension: "model".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(usage["groups"].as_array().unwrap().len(), 2);
        let model_one = usage["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|group| group["dimension_id"] == "model-one")
            .unwrap();
        assert_eq!(model_one["input_tokens"], 10);
        assert_eq!(usage["cost_available"], false);
        assert!(matches!(
            service
                .usage_report(
                    &context,
                    UsageQuery {
                        from,
                        to,
                        dimension: "request_id".to_owned(),
                    },
                )
                .await,
            Err(AdminError::InvalidRequest)
        ));

        let first_page = service
            .request_page(
                &context,
                ReportListQuery {
                    from,
                    to,
                    limit: Some(1),
                    cursor: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(first_page.items.len(), 1);
        let second_page = service
            .request_page(
                &context,
                ReportListQuery {
                    from,
                    to,
                    limit: Some(1),
                    cursor: first_page.next_cursor,
                },
            )
            .await
            .unwrap();
        assert_eq!(second_page.items.len(), 1);
        assert_ne!(first_page.items[0]["id"], second_page.items[0]["id"]);

        let detail = service.request_report(&context, request_one).await.unwrap();
        assert_eq!(detail["attempts"].as_array().unwrap().len(), 1);
        assert_eq!(detail["usage"].as_array().unwrap().len(), 1);
        assert_eq!(detail["usage"][0]["input_tokens"], 10);
        assert_eq!(
            detail["retry_fallback_history"]["same_request_retry"],
            false
        );
        assert_eq!(
            detail["retry_fallback_history"]["events"][0]["attempt_kind"],
            "initial"
        );
        assert!(matches!(
            service.request_report(&context, Uuid::from_u128(103)).await,
            Err(AdminError::NotFound)
        ));

        let audit = service
            .audit_page(
                &context,
                ReportListQuery {
                    from,
                    to,
                    limit: Some(10),
                    cursor: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(audit.items.len(), 1);
        assert_eq!(audit.items[0]["before"]["key_prefix"], "aigw_test");
        assert!(audit.items[0]["before"].get("secret_hash").is_none());
        assert!(audit.items[0]["before"].get("fingerprint").is_none());

        assert!(service.identity_map(&context).await.unwrap()["paths"].is_array());
        let catalog = service.catalog_report(&context).await.unwrap();
        assert_eq!(catalog["aliases"][0]["public_model_id"], "local-code");
        assert_eq!(
            catalog["aliases"][0]["provider_model_id"],
            "qwen3-coder:30b"
        );
        service.governance_report(&context).await.unwrap();
        assert!(service.system_health_report(&context, false).await.is_ok());
    }
}
