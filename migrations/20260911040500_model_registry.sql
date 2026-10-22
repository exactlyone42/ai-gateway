CREATE FUNCTION valid_gateway_operations(value TEXT[]) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT cardinality(value) > 0
       AND NOT EXISTS (
           SELECT 1 FROM unnest(value) AS operation
           WHERE operation NOT IN (
               'anthropic_messages', 'anthropic_count_tokens',
               'openai_responses', 'openai_chat_completions'
           )
       );
$$;

ALTER TABLE providers
    DROP CONSTRAINT providers_lifecycle_state_check,
    ADD COLUMN supported_operations TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    ADD COLUMN enabled BOOLEAN GENERATED ALWAYS AS (lifecycle_state = 'active') STORED,
    ADD CONSTRAINT providers_lifecycle_state_check
        CHECK (lifecycle_state IN ('draft', 'active', 'disabled', 'archived')),
    ADD CONSTRAINT providers_active_operations_check
        CHECK (lifecycle_state <> 'active' OR valid_gateway_operations(supported_operations)),
    ADD CONSTRAINT providers_non_nil_tenant_check
        CHECK (tenant_id <> '00000000-0000-0000-0000-000000000000'::uuid);

ALTER TABLE models
    DROP CONSTRAINT models_lifecycle_state_check,
    ADD COLUMN display_name TEXT,
    ADD COLUMN description TEXT,
    ADD COLUMN supported_operations TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    ADD COLUMN enabled BOOLEAN GENERATED ALWAYS AS (lifecycle_state = 'active') STORED,
    ADD CONSTRAINT models_lifecycle_state_check
        CHECK (lifecycle_state IN ('draft', 'active', 'disabled', 'archived')),
    ADD CONSTRAINT models_public_id_check
        CHECK (public_model_id = btrim(public_model_id)
            AND length(public_model_id) BETWEEN 1 AND 255
            AND public_model_id !~ '^quality:'),
    ADD CONSTRAINT models_provider_id_check
        CHECK (provider_model_id = btrim(provider_model_id)
            AND length(provider_model_id) BETWEEN 1 AND 255),
    ADD CONSTRAINT models_metadata_size_check
        CHECK (octet_length(cost_metadata::text) <= 4096
            AND (description IS NULL OR length(description) <= 1024)
            AND (latency_class IS NULL OR length(latency_class) <= 64)),
    ADD CONSTRAINT models_active_operations_check
        CHECK (lifecycle_state <> 'active' OR valid_gateway_operations(supported_operations)),
    ADD CONSTRAINT models_non_nil_tenant_check
        CHECK (tenant_id <> '00000000-0000-0000-0000-000000000000'::uuid);

UPDATE models
SET display_name = public_model_id
WHERE display_name IS NULL;

ALTER TABLE models
    ALTER COLUMN display_name SET NOT NULL,
    ADD CONSTRAINT models_display_name_check CHECK (btrim(display_name) <> ''),
    ADD CONSTRAINT models_description_check
        CHECK (description IS NULL OR btrim(description) <> '');

CREATE TABLE model_aliases (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL CHECK (tenant_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    operation TEXT NOT NULL CHECK (operation IN (
        'anthropic_messages', 'anthropic_count_tokens',
        'openai_responses', 'openai_chat_completions'
    )),
    quality_tier TEXT NOT NULL CHECK (
        quality_tier IN ('economy', 'standard', 'advanced', 'premium')
    ),
    model_id UUID NOT NULL,
    lifecycle_state TEXT NOT NULL DEFAULT 'draft'
        CHECK (lifecycle_state IN ('draft', 'active', 'disabled', 'archived')),
    enabled BOOLEAN GENERATED ALWAYS AS (lifecycle_state = 'active') STORED,
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, model_id) REFERENCES models (tenant_id, id)
);

CREATE UNIQUE INDEX model_aliases_active_selector_idx
    ON model_aliases (tenant_id, operation, quality_tier)
    WHERE lifecycle_state = 'active';
CREATE INDEX model_aliases_model_idx
    ON model_aliases (tenant_id, model_id, lifecycle_state);
CREATE INDEX models_active_public_idx
    ON models (tenant_id, public_model_id, provider_id)
    WHERE lifecycle_state = 'active';
CREATE INDEX providers_active_key_idx
    ON providers (tenant_id, provider_key)
    WHERE lifecycle_state = 'active';

ALTER TABLE gateway_requests
    ADD COLUMN selector_kind TEXT CHECK (selector_kind IN ('model', 'quality')),
    ADD COLUMN selector_value TEXT,
    ADD COLUMN selected_alias_id UUID,
    ADD COLUMN selected_alias_version BIGINT CHECK (
        selected_alias_version IS NULL OR selected_alias_version > 0
    ),
    ADD COLUMN selected_model_id UUID,
    ADD COLUMN selected_model_version BIGINT CHECK (
        selected_model_version IS NULL OR selected_model_version > 0
    ),
    ADD COLUMN selected_provider_id UUID,
    ADD COLUMN selected_provider_version BIGINT CHECK (
        selected_provider_version IS NULL OR selected_provider_version > 0
    ),
    ADD COLUMN selected_provider_model_id TEXT,
    ADD COLUMN selected_cost_metadata JSONB CHECK (
        selected_cost_metadata IS NULL OR jsonb_typeof(selected_cost_metadata) = 'object'
    ),
    ADD FOREIGN KEY (tenant_id, selected_alias_id)
        REFERENCES model_aliases (tenant_id, id),
    ADD FOREIGN KEY (tenant_id, selected_model_id)
        REFERENCES models (tenant_id, id),
    ADD FOREIGN KEY (tenant_id, selected_provider_id)
        REFERENCES providers (tenant_id, id),
    ADD CHECK (
        (selector_kind IS NULL) = (selector_value IS NULL)
    );

ALTER TABLE provider_attempts
    ADD COLUMN provider_id UUID,
    ADD COLUMN provider_version BIGINT CHECK (provider_version IS NULL OR provider_version > 0),
    ADD COLUMN model_id UUID,
    ADD COLUMN model_version BIGINT CHECK (model_version IS NULL OR model_version > 0),
    ADD COLUMN provider_model_id TEXT,
    ADD COLUMN cost_metadata JSONB CHECK (
        cost_metadata IS NULL OR jsonb_typeof(cost_metadata) = 'object'
    ),
    ADD FOREIGN KEY (tenant_id, provider_id) REFERENCES providers (tenant_id, id),
    ADD FOREIGN KEY (tenant_id, model_id) REFERENCES models (tenant_id, id);

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
        selected_cost_metadata
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
        r.selected_provider_model_id, r.selected_cost_metadata
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
        selected_provider_model_id TEXT, selected_cost_metadata JSONB
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
