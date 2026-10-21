ALTER TABLE quotas
    DROP CONSTRAINT quotas_lifecycle_state_check,
    ADD COLUMN enabled BOOLEAN GENERATED ALWAYS AS (lifecycle_state = 'active') STORED,
    ADD CONSTRAINT quotas_lifecycle_state_check
        CHECK (lifecycle_state IN ('draft', 'active', 'disabled', 'archived')),
    ADD CONSTRAINT quotas_non_nil_tenant_check
        CHECK (tenant_id <> '00000000-0000-0000-0000-000000000000'::uuid) NOT VALID,
    ADD CONSTRAINT quotas_key_size_check CHECK (char_length(quota_key) <= 200) NOT VALID,
    ADD CONSTRAINT quotas_configuration_size_check
        CHECK (octet_length(configuration::text) <= 16384) NOT VALID;

UPDATE quotas q
SET credential_id = COALESCE(c.rotation_root_id, c.id)
FROM credentials c
WHERE (c.tenant_id, c.id) = (q.tenant_id, q.credential_id)
  AND q.credential_id IS DISTINCT FROM COALESCE(c.rotation_root_id, c.id);

CREATE UNIQUE INDEX quotas_active_scope_idx
    ON quotas (
        tenant_id, quota_key,
        (CASE
            WHEN user_id IS NOT NULL THEN 'user'
            WHEN device_id IS NOT NULL THEN 'device'
            WHEN client_id IS NOT NULL THEN 'client'
            WHEN agent_id IS NOT NULL THEN 'agent'
            WHEN credential_id IS NOT NULL THEN 'credential'
            WHEN model_id IS NOT NULL THEN 'model'
            WHEN provider_id IS NOT NULL THEN 'provider'
            ELSE 'global'
        END),
        (COALESCE(
            user_id, device_id, client_id, agent_id, credential_id, model_id, provider_id,
            '00000000-0000-0000-0000-000000000000'::uuid
        ))
    )
    WHERE lifecycle_state = 'active';

CREATE TABLE quota_windows (
    tenant_id UUID NOT NULL,
    quota_id UUID NOT NULL,
    limit_kind TEXT NOT NULL CHECK (limit_kind IN (
        'requests_per_minute', 'estimated_tokens_per_minute',
        'tokens_per_hour', 'tokens_per_day',
        'cost_per_day_microusd', 'cost_per_month_microusd'
    )),
    window_start TIMESTAMPTZ NOT NULL,
    window_end TIMESTAMPTZ NOT NULL,
    reserved_requests BIGINT NOT NULL DEFAULT 0 CHECK (reserved_requests >= 0),
    reserved_tokens BIGINT NOT NULL DEFAULT 0 CHECK (reserved_tokens >= 0),
    reserved_cost_microusd BIGINT NOT NULL DEFAULT 0 CHECK (reserved_cost_microusd >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, quota_id, limit_kind, window_start),
    FOREIGN KEY (tenant_id, quota_id) REFERENCES quotas (tenant_id, id),
    CHECK (window_end > window_start)
);

CREATE INDEX quota_windows_current_idx
    ON quota_windows (tenant_id, window_end DESC, quota_id);

CREATE TABLE quota_reservations (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    gateway_request_id UUID NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('active', 'expired', 'reconciled')),
    estimated_tokens BIGINT NOT NULL CHECK (estimated_tokens >= 0),
    estimated_cost_microusd BIGINT NOT NULL CHECK (estimated_cost_microusd >= 0),
    actual_tokens BIGINT CHECK (actual_tokens IS NULL OR actual_tokens >= 0),
    actual_cost_microusd BIGINT CHECK (actual_cost_microusd IS NULL OR actual_cost_microusd >= 0),
    decision JSONB NOT NULL CHECK (
        jsonb_typeof(decision) = 'object' AND octet_length(decision::text) <= 65536
    ),
    allocations JSONB NOT NULL CHECK (
        jsonb_typeof(allocations) = 'array' AND octet_length(allocations::text) <= 32768
    ),
    expires_at TIMESTAMPTZ NOT NULL,
    reconciled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, gateway_request_id),
    CHECK (expires_at > created_at),
    CHECK ((state = 'reconciled') = (reconciled_at IS NOT NULL))
);

CREATE INDEX quota_reservations_active_idx
    ON quota_reservations (tenant_id, expires_at) WHERE state = 'active';

CREATE TABLE quota_reconciliation_events (
    tenant_id UUID NOT NULL,
    reservation_id UUID NOT NULL,
    event_kind TEXT NOT NULL CHECK (event_kind = 'final'),
    request_status TEXT NOT NULL,
    actual_tokens BIGINT NOT NULL CHECK (actual_tokens >= 0),
    actual_cost_microusd BIGINT NOT NULL CHECK (actual_cost_microusd >= 0),
    occurred_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (tenant_id, reservation_id, event_kind),
    FOREIGN KEY (tenant_id, reservation_id)
        REFERENCES quota_reservations (tenant_id, id)
);

CREATE TRIGGER quota_reconciliation_events_append_only
    BEFORE UPDATE OR DELETE ON quota_reconciliation_events
    FOR EACH ROW EXECUTE FUNCTION reject_ledger_mutation();
CREATE TRIGGER quota_reconciliation_events_no_truncate
    BEFORE TRUNCATE ON quota_reconciliation_events
    FOR EACH STATEMENT EXECUTE FUNCTION reject_ledger_mutation();

ALTER TABLE gateway_requests
    ADD COLUMN quota_reservation_id UUID,
    ADD COLUMN quota_decision JSONB,
    ADD CONSTRAINT gateway_requests_quota_reservation_fk
        FOREIGN KEY (tenant_id, quota_reservation_id)
        REFERENCES quota_reservations (tenant_id, id) NOT VALID,
    ADD CONSTRAINT gateway_requests_quota_decision_check CHECK (
        quota_decision IS NULL OR (
            jsonb_typeof(quota_decision) = 'object'
            AND octet_length(quota_decision::text) <= 65536
        )
    ) NOT VALID;

CREATE OR REPLACE FUNCTION ingest_usage_envelopes(payload JSONB) RETURNS VOID
LANGUAGE plpgsql AS $$
BEGIN
    IF jsonb_typeof(payload) <> 'array' THEN
        RAISE EXCEPTION 'usage envelope payload must be an array';
    END IF;

    PERFORM pg_advisory_xact_lock(hashtextextended(tenant_id::text, 12012))
    FROM (
        SELECT DISTINCT (envelope->'request'->>'tenant_id')::uuid AS tenant_id
        FROM jsonb_array_elements(payload) AS envelope
        WHERE envelope->'request'->>'tenant_id' IS NOT NULL
        ORDER BY tenant_id
    ) tenants;

    INSERT INTO gateway_requests (
        id, tenant_id, request_id, user_id, device_id, client_id, agent_id,
        credential_id, session_id, trace_id, workload_class, urgency, quality,
        budget_class, requested_model, selected_model, selected_provider,
        estimated_tokens, queue_duration, status, created_at, started_at, completed_at,
        operation, client_agent_id, parent_agent_id, request_received_at,
        queue_entered_at, queue_left_at, client_first_byte_at, total_latency,
        gateway_overhead, selector_kind, selector_value, selected_alias_id,
        selected_alias_version, selected_model_id, selected_model_version,
        selected_provider_id, selected_provider_version, selected_provider_model_id,
        selected_cost_metadata, deadline_at, max_queue_time, policy_decision,
        quota_reservation_id, quota_decision
    )
    SELECT
        r.id, r.tenant_id, r.request_id, r.user_id, r.device_id, r.client_id,
        r.agent_id, r.credential_id, r.session_id, r.trace_id, r.workload_class,
        r.urgency, r.quality, r.budget_class, r.requested_model,
        r.selected_model, r.selected_provider, r.estimated_tokens,
        CASE WHEN r.queue_duration_ms IS NULL THEN NULL
             ELSE r.queue_duration_ms * INTERVAL '1 millisecond' END,
        r.status, r.created_at, r.started_at, r.completed_at,
        COALESCE(r.operation, 'chat_completions'), r.client_agent_id,
        r.parent_agent_id, COALESCE(r.request_received_at, r.created_at),
        r.queue_entered_at, r.queue_left_at, r.client_first_byte_at,
        CASE WHEN r.total_latency_ms IS NULL THEN NULL
             ELSE r.total_latency_ms * INTERVAL '1 millisecond' END,
        CASE WHEN r.gateway_overhead_ms IS NULL THEN NULL
             ELSE r.gateway_overhead_ms * INTERVAL '1 millisecond' END,
        r.selector_kind, r.selector_value, r.selected_alias_id,
        r.selected_alias_version, r.selected_model_id, r.selected_model_version,
        r.selected_provider_id, r.selected_provider_version,
        r.selected_provider_model_id, r.selected_cost_metadata, r.deadline_at,
        CASE WHEN r.max_queue_time_ms IS NULL THEN NULL
             ELSE r.max_queue_time_ms * INTERVAL '1 millisecond' END,
        r.policy_decision, r.quota_reservation_id, r.quota_decision
    FROM jsonb_array_elements(payload) AS envelope
    CROSS JOIN LATERAL jsonb_to_record(envelope->'request') AS r(
        id UUID, tenant_id UUID, request_id TEXT, user_id UUID, device_id UUID,
        client_id UUID, agent_id UUID, credential_id UUID, session_id TEXT,
        trace_id TEXT, workload_class TEXT, urgency TEXT, quality TEXT,
        budget_class TEXT, requested_model TEXT, selected_model TEXT,
        selected_provider TEXT, estimated_tokens BIGINT, queue_duration_ms BIGINT,
        status TEXT, created_at TIMESTAMPTZ, started_at TIMESTAMPTZ,
        completed_at TIMESTAMPTZ, operation TEXT, client_agent_id TEXT,
        parent_agent_id TEXT, request_received_at TIMESTAMPTZ,
        queue_entered_at TIMESTAMPTZ, queue_left_at TIMESTAMPTZ,
        client_first_byte_at TIMESTAMPTZ, total_latency_ms BIGINT,
        gateway_overhead_ms BIGINT, selector_kind TEXT, selector_value TEXT,
        selected_alias_id UUID, selected_alias_version BIGINT,
        selected_model_id UUID, selected_model_version BIGINT,
        selected_provider_id UUID, selected_provider_version BIGINT,
        selected_provider_model_id TEXT, selected_cost_metadata JSONB,
        deadline_at TIMESTAMPTZ, max_queue_time_ms BIGINT, policy_decision JSONB,
        quota_reservation_id UUID, quota_decision JSONB,
        quota_actual_tokens BIGINT, quota_actual_cost_microusd BIGINT
    )
    ON CONFLICT DO NOTHING;

    INSERT INTO provider_attempts (
        id, tenant_id, gateway_request_id, provider, model,
        provider_request_id, provider_response_id, attempt_number, started_at,
        first_token_at, completed_at, status, error_type,
        estimated_usage, actual_usage, provider_request_at,
        provider_first_byte_at, provider_ttft, provider_id, provider_version,
        model_id, model_version, provider_model_id, cost_metadata
    )
    SELECT
        a.id, a.tenant_id, a.gateway_request_id, a.provider, a.model,
        a.provider_request_id, a.provider_response_id, a.attempt_number,
        a.started_at, a.first_token_at, a.completed_at, a.status, a.error_type,
        a.estimated_usage, a.actual_usage,
        COALESCE(a.provider_request_at, a.started_at),
        COALESCE(a.provider_first_byte_at, a.first_token_at),
        CASE WHEN a.provider_ttft_ms IS NULL THEN NULL
             ELSE a.provider_ttft_ms * INTERVAL '1 millisecond' END,
        a.provider_id, a.provider_version, a.model_id, a.model_version,
        a.provider_model_id, a.cost_metadata
    FROM jsonb_array_elements(payload) AS envelope
    CROSS JOIN LATERAL jsonb_array_elements(envelope->'attempts') AS attempt
    CROSS JOIN LATERAL jsonb_to_record(attempt) AS a(
        id UUID, tenant_id UUID, gateway_request_id UUID, provider TEXT,
        model TEXT, provider_request_id TEXT, provider_response_id TEXT,
        attempt_number INTEGER, started_at TIMESTAMPTZ,
        first_token_at TIMESTAMPTZ, completed_at TIMESTAMPTZ, status TEXT,
        error_type TEXT, estimated_usage JSONB, actual_usage JSONB,
        provider_request_at TIMESTAMPTZ, provider_first_byte_at TIMESTAMPTZ,
        provider_ttft_ms BIGINT, provider_id UUID, provider_version BIGINT,
        model_id UUID, model_version BIGINT, provider_model_id TEXT,
        cost_metadata JSONB
    )
    ON CONFLICT DO NOTHING;

    INSERT INTO usage_records (
        id, tenant_id, gateway_request_id, provider_attempt_id, source,
        input_tokens, output_tokens, cached_input_tokens,
        cache_write_input_tokens, reasoning_tokens, cost_microusd, recorded_at
    )
    SELECT
        u.id, u.tenant_id, u.gateway_request_id, u.provider_attempt_id, u.source,
        u.input_tokens, u.output_tokens, u.cached_input_tokens,
        u.cache_write_input_tokens, u.reasoning_tokens, u.cost_microusd,
        u.recorded_at
    FROM jsonb_array_elements(payload) AS envelope
    CROSS JOIN LATERAL jsonb_array_elements(envelope->'usage_records') AS usage
    CROSS JOIN LATERAL jsonb_to_record(usage) AS u(
        id UUID, tenant_id UUID, gateway_request_id UUID,
        provider_attempt_id UUID, source TEXT, input_tokens BIGINT,
        output_tokens BIGINT, cached_input_tokens BIGINT,
        cache_write_input_tokens BIGINT, reasoning_tokens BIGINT,
        cost_microusd BIGINT, recorded_at TIMESTAMPTZ
    )
    ON CONFLICT DO NOTHING;

    WITH incoming AS (
        SELECT DISTINCT ON (r.tenant_id, r.quota_reservation_id)
               r.tenant_id, r.quota_reservation_id AS reservation_id,
               r.status, r.quota_actual_tokens AS actual_tokens,
               r.quota_actual_cost_microusd AS actual_cost_microusd,
               r.completed_at
        FROM jsonb_array_elements(payload) AS envelope
        CROSS JOIN LATERAL jsonb_to_record(envelope->'request') AS r(
            tenant_id UUID, quota_reservation_id UUID, status TEXT,
            quota_actual_tokens BIGINT, quota_actual_cost_microusd BIGINT,
            completed_at TIMESTAMPTZ
        )
        WHERE r.quota_reservation_id IS NOT NULL
          AND r.quota_actual_tokens IS NOT NULL
          AND r.quota_actual_cost_microusd IS NOT NULL
        ORDER BY r.tenant_id, r.quota_reservation_id, r.completed_at DESC
    ), inserted AS (
        INSERT INTO quota_reconciliation_events (
            tenant_id, reservation_id, event_kind, request_status,
            actual_tokens, actual_cost_microusd, occurred_at
        )
        SELECT tenant_id, reservation_id, 'final', status,
               actual_tokens, actual_cost_microusd, completed_at
        FROM incoming
        ON CONFLICT DO NOTHING
        RETURNING tenant_id, reservation_id, actual_tokens,
                  actual_cost_microusd, occurred_at
    ), deltas AS (
        SELECT i.tenant_id,
               (a->>'quota_id')::uuid AS quota_id,
               a->>'limit_kind' AS limit_kind,
               (a->>'window_start')::timestamptz AS window_start,
               CASE WHEN (a->>'reserved_tokens')::bigint > 0
                    THEN i.actual_tokens - (a->>'reserved_tokens')::bigint ELSE 0 END AS tokens,
               CASE WHEN (a->>'reserved_cost_microusd')::bigint > 0
                    THEN i.actual_cost_microusd - (a->>'reserved_cost_microusd')::bigint ELSE 0 END AS cost
        FROM inserted i
        JOIN quota_reservations r
          ON (r.tenant_id, r.id) = (i.tenant_id, i.reservation_id)
        CROSS JOIN LATERAL jsonb_array_elements(r.allocations) AS a
    ), updated_windows AS (
        UPDATE quota_windows w SET
            reserved_tokens = GREATEST(0, w.reserved_tokens + d.tokens),
            reserved_cost_microusd = GREATEST(0, w.reserved_cost_microusd + d.cost),
            updated_at = now()
        FROM deltas d
        WHERE (w.tenant_id, w.quota_id, w.limit_kind, w.window_start)
            = (d.tenant_id, d.quota_id, d.limit_kind, d.window_start)
        RETURNING w.quota_id
    )
    UPDATE quota_reservations r SET
        state = 'reconciled', actual_tokens = i.actual_tokens,
        actual_cost_microusd = i.actual_cost_microusd,
        reconciled_at = i.occurred_at
    FROM inserted i
    WHERE (r.tenant_id, r.id) = (i.tenant_id, i.reservation_id);
END;
$$;

CREATE FUNCTION recompute_quota_windows(target_tenant UUID) RETURNS BIGINT
LANGUAGE plpgsql AS $$
DECLARE
    rebuilt BIGINT;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(target_tenant::text, 12012));
    DELETE FROM quota_windows WHERE tenant_id = target_tenant;
    INSERT INTO quota_windows (
        tenant_id, quota_id, limit_kind, window_start, window_end,
        reserved_requests, reserved_tokens, reserved_cost_microusd
    )
    SELECT r.tenant_id, a.quota_id, a.limit_kind, a.window_start, max(a.window_end),
           sum(a.reserved_requests),
           sum(CASE WHEN r.state = 'reconciled' AND a.reserved_tokens > 0
                    THEN r.actual_tokens ELSE a.reserved_tokens END),
           sum(CASE WHEN r.state = 'reconciled' AND a.reserved_cost_microusd > 0
                    THEN r.actual_cost_microusd ELSE a.reserved_cost_microusd END)
    FROM quota_reservations r
    CROSS JOIN LATERAL jsonb_to_recordset(r.allocations) AS a(
        quota_id UUID, limit_kind TEXT, window_start TIMESTAMPTZ, window_end TIMESTAMPTZ,
        reserved_requests BIGINT, reserved_tokens BIGINT, reserved_cost_microusd BIGINT
    )
    WHERE r.tenant_id = target_tenant
    GROUP BY r.tenant_id, a.quota_id, a.limit_kind, a.window_start;
    GET DIAGNOSTICS rebuilt = ROW_COUNT;
    RETURN rebuilt;
END;
$$;
