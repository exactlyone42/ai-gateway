ALTER TABLE policies
    DROP CONSTRAINT policies_lifecycle_state_check,
    ADD COLUMN enabled BOOLEAN GENERATED ALWAYS AS (lifecycle_state = 'active') STORED,
    ADD CONSTRAINT policies_lifecycle_state_check
        CHECK (lifecycle_state IN ('draft', 'active', 'disabled', 'archived')),
    ADD CONSTRAINT policies_non_nil_tenant_check
        CHECK (tenant_id <> '00000000-0000-0000-0000-000000000000'::uuid) NOT VALID,
    ADD CONSTRAINT policies_key_size_check
        CHECK (char_length(policy_key) <= 200) NOT VALID,
    ADD CONSTRAINT policies_configuration_size_check
        CHECK (octet_length(configuration::text) <= 16384) NOT VALID;

ALTER TABLE credentials
    ADD COLUMN rotation_root_id UUID,
    ADD CONSTRAINT credentials_rotation_root_fk
        FOREIGN KEY (tenant_id, rotation_root_id)
        REFERENCES credentials (tenant_id, id) NOT VALID;

CREATE INDEX credentials_rotation_root_idx
    ON credentials (tenant_id, rotation_root_id)
    WHERE rotation_root_id IS NOT NULL;

CREATE UNIQUE INDEX policies_active_scope_idx
    ON policies (
        tenant_id,
        (CASE
            WHEN user_id IS NOT NULL THEN 'user'
            WHEN device_id IS NOT NULL THEN 'device'
            WHEN client_id IS NOT NULL THEN 'client'
            WHEN agent_id IS NOT NULL THEN 'agent'
            WHEN credential_id IS NOT NULL THEN 'credential'
            ELSE 'global'
        END),
        (COALESCE(
            user_id, device_id, client_id, agent_id, credential_id,
            '00000000-0000-0000-0000-000000000000'::uuid
        ))
    )
    WHERE lifecycle_state = 'active';

ALTER TABLE gateway_requests
    ADD COLUMN deadline_at TIMESTAMPTZ,
    ADD COLUMN max_queue_time INTERVAL,
    ADD COLUMN policy_decision JSONB,
    ADD CONSTRAINT gateway_requests_max_queue_time_check CHECK (
        max_queue_time IS NULL OR (max_queue_time > INTERVAL '0' AND max_queue_time <= INTERVAL '1 day')
    ) NOT VALID,
    ADD CONSTRAINT gateway_requests_policy_decision_check CHECK (
        policy_decision IS NULL OR (
            jsonb_typeof(policy_decision) = 'object'
            AND octet_length(policy_decision::text) <= 8192
        )
    ) NOT VALID,
    ADD CONSTRAINT gateway_requests_workload_class_check CHECK (workload_class IS NULL OR workload_class IN (
        'critical', 'interactive', 'foreground', 'automation', 'background', 'batch'
    )) NOT VALID,
    ADD CONSTRAINT gateway_requests_urgency_check
        CHECK (urgency IS NULL OR urgency IN ('immediate', 'normal', 'low', 'idle')) NOT VALID,
    ADD CONSTRAINT gateway_requests_quality_check
        CHECK (quality IS NULL OR quality IN ('economy', 'standard', 'advanced', 'premium')) NOT VALID,
    ADD CONSTRAINT gateway_requests_budget_class_check
        CHECK (budget_class IS NULL OR budget_class IN ('cheap', 'normal', 'unrestricted')) NOT VALID;

CREATE OR REPLACE FUNCTION ingest_usage_envelopes(payload JSONB) RETURNS VOID
LANGUAGE plpgsql AS $$
BEGIN
    IF jsonb_typeof(payload) <> 'array' THEN
        RAISE EXCEPTION 'usage envelope payload must be an array';
    END IF;

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
        selected_cost_metadata, deadline_at, max_queue_time, policy_decision
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
        r.policy_decision
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
        deadline_at TIMESTAMPTZ, max_queue_time_ms BIGINT, policy_decision JSONB
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
END;
$$;
