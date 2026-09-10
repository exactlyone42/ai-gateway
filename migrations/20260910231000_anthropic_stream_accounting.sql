ALTER TABLE gateway_requests
    ADD COLUMN operation TEXT NOT NULL DEFAULT 'chat_completions',
    ADD COLUMN client_agent_id TEXT,
    ADD COLUMN parent_agent_id TEXT,
    ADD COLUMN request_received_at TIMESTAMPTZ,
    ADD COLUMN queue_entered_at TIMESTAMPTZ,
    ADD COLUMN queue_left_at TIMESTAMPTZ,
    ADD COLUMN client_first_byte_at TIMESTAMPTZ,
    ADD COLUMN total_latency INTERVAL CHECK (total_latency >= INTERVAL '0 seconds'),
    ADD COLUMN gateway_overhead INTERVAL CHECK (gateway_overhead >= INTERVAL '0 seconds');

ALTER TABLE gateway_requests
    ALTER COLUMN queue_duration DROP NOT NULL,
    ALTER COLUMN queue_duration DROP DEFAULT;

ALTER TABLE provider_attempts
    ADD COLUMN provider_request_at TIMESTAMPTZ,
    ADD COLUMN provider_first_byte_at TIMESTAMPTZ,
    ADD COLUMN provider_ttft INTERVAL CHECK (provider_ttft >= INTERVAL '0 seconds');

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
        gateway_overhead
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
             ELSE r.gateway_overhead_ms * INTERVAL '1 millisecond' END
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
        gateway_overhead_ms BIGINT
    )
    ON CONFLICT DO NOTHING;

    INSERT INTO provider_attempts (
        id, tenant_id, gateway_request_id, provider, model,
        provider_request_id, provider_response_id, attempt_number, started_at,
        first_token_at, completed_at, status, error_type,
        estimated_usage, actual_usage, provider_request_at,
        provider_first_byte_at, provider_ttft
    )
    SELECT
        a.id, a.tenant_id, a.gateway_request_id, a.provider, a.model,
        a.provider_request_id, a.provider_response_id, a.attempt_number,
        a.started_at, a.first_token_at, a.completed_at, a.status, a.error_type,
        a.estimated_usage, a.actual_usage,
        COALESCE(a.provider_request_at, a.started_at),
        COALESCE(a.provider_first_byte_at, a.first_token_at),
        CASE WHEN a.provider_ttft_ms IS NULL THEN NULL
             ELSE a.provider_ttft_ms * INTERVAL '1 millisecond' END
    FROM jsonb_array_elements(payload) AS envelope
    CROSS JOIN LATERAL jsonb_array_elements(envelope->'attempts') AS attempt
    CROSS JOIN LATERAL jsonb_to_record(attempt) AS a(
        id UUID, tenant_id UUID, gateway_request_id UUID, provider TEXT,
        model TEXT, provider_request_id TEXT, provider_response_id TEXT,
        attempt_number INTEGER, started_at TIMESTAMPTZ,
        first_token_at TIMESTAMPTZ, completed_at TIMESTAMPTZ, status TEXT,
        error_type TEXT, estimated_usage JSONB, actual_usage JSONB,
        provider_request_at TIMESTAMPTZ, provider_first_byte_at TIMESTAMPTZ,
        provider_ttft_ms BIGINT
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
