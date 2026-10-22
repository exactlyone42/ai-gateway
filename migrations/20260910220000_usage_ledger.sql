CREATE FUNCTION valid_token_usage(value JSONB) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT jsonb_typeof(value) = 'object'
       AND NOT EXISTS (
           SELECT 1 FROM jsonb_object_keys(value) AS key
           WHERE key NOT IN (
               'input_tokens', 'output_tokens', 'cached_input_tokens',
               'cache_write_input_tokens', 'reasoning_tokens'
           )
       )
       AND NOT EXISTS (
           SELECT 1 FROM jsonb_each(value) AS entry
           WHERE jsonb_typeof(entry.value) <> 'number'
              OR (entry.value #>> '{}')::numeric < 0
       );
$$;

CREATE TABLE gateway_requests (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    request_id TEXT NOT NULL CHECK (btrim(request_id) <> ''),
    user_id UUID,
    device_id UUID,
    client_id UUID,
    agent_id UUID,
    credential_id UUID,
    session_id TEXT,
    trace_id TEXT,
    workload_class TEXT,
    urgency TEXT,
    quality TEXT,
    budget_class TEXT,
    requested_model TEXT,
    selected_model TEXT,
    selected_provider TEXT,
    estimated_tokens BIGINT CHECK (estimated_tokens IS NULL OR estimated_tokens >= 0),
    queue_duration INTERVAL NOT NULL DEFAULT INTERVAL '0 seconds'
        CHECK (queue_duration >= INTERVAL '0 seconds'),
    status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, user_id) REFERENCES users (tenant_id, id),
    FOREIGN KEY (tenant_id, device_id) REFERENCES devices (tenant_id, id),
    FOREIGN KEY (tenant_id, client_id) REFERENCES clients (tenant_id, id),
    FOREIGN KEY (tenant_id, agent_id) REFERENCES agents (tenant_id, id),
    FOREIGN KEY (tenant_id, credential_id) REFERENCES credentials (tenant_id, id),
    CHECK (started_at IS NULL OR started_at >= created_at),
    CHECK (completed_at IS NULL OR completed_at >= COALESCE(started_at, created_at))
);

CREATE INDEX gateway_requests_created_at_idx
    ON gateway_requests (tenant_id, created_at DESC);
CREATE INDEX gateway_requests_principal_idx
    ON gateway_requests (tenant_id, user_id, device_id, client_id, agent_id, credential_id);

CREATE TABLE provider_attempts (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    gateway_request_id UUID NOT NULL,
    provider TEXT NOT NULL CHECK (btrim(provider) <> ''),
    model TEXT NOT NULL CHECK (btrim(model) <> ''),
    provider_request_id TEXT,
    provider_response_id TEXT,
    attempt_number INTEGER NOT NULL CHECK (attempt_number > 0),
    started_at TIMESTAMPTZ NOT NULL,
    first_token_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    status TEXT NOT NULL,
    error_type TEXT,
    estimated_usage JSONB NOT NULL DEFAULT '{}'::jsonb
        CHECK (valid_token_usage(estimated_usage)),
    actual_usage JSONB NOT NULL DEFAULT '{}'::jsonb
        CHECK (valid_token_usage(actual_usage)),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, gateway_request_id, id),
    UNIQUE (gateway_request_id, attempt_number),
    FOREIGN KEY (tenant_id, gateway_request_id)
        REFERENCES gateway_requests (tenant_id, id),
    CHECK (first_token_at IS NULL OR first_token_at >= started_at),
    CHECK (completed_at IS NULL OR completed_at >= started_at)
);

CREATE INDEX provider_attempts_request_idx
    ON provider_attempts (tenant_id, gateway_request_id, attempt_number);

CREATE TABLE usage_records (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    gateway_request_id UUID NOT NULL,
    provider_attempt_id UUID NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('estimated', 'provider_reported')),
    input_tokens BIGINT NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens BIGINT NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    cached_input_tokens BIGINT NOT NULL DEFAULT 0 CHECK (cached_input_tokens >= 0),
    cache_write_input_tokens BIGINT NOT NULL DEFAULT 0 CHECK (cache_write_input_tokens >= 0),
    reasoning_tokens BIGINT NOT NULL DEFAULT 0 CHECK (reasoning_tokens >= 0),
    cost_microusd BIGINT CHECK (cost_microusd IS NULL OR cost_microusd >= 0),
    recorded_at TIMESTAMPTZ NOT NULL,
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, provider_attempt_id, source),
    FOREIGN KEY (tenant_id, gateway_request_id)
        REFERENCES gateway_requests (tenant_id, id),
    FOREIGN KEY (tenant_id, gateway_request_id, provider_attempt_id)
        REFERENCES provider_attempts (tenant_id, gateway_request_id, id)
);

CREATE INDEX usage_records_request_idx
    ON usage_records (tenant_id, gateway_request_id, recorded_at);

CREATE TABLE audit_logs (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    actor_credential_id UUID,
    action TEXT NOT NULL CHECK (btrim(action) <> ''),
    target_type TEXT NOT NULL CHECK (btrim(target_type) <> ''),
    target_id UUID,
    before_state JSONB,
    after_state JSONB,
    occurred_at TIMESTAMPTZ NOT NULL,
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, actor_credential_id) REFERENCES credentials (tenant_id, id)
);

CREATE INDEX audit_logs_occurred_at_idx
    ON audit_logs (tenant_id, occurred_at DESC);

CREATE FUNCTION reject_identity_deletion() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is retained for ledger referential integrity; revoke or deactivate it instead', TG_TABLE_NAME;
END;
$$;

CREATE TRIGGER users_no_delete BEFORE DELETE ON users
    FOR EACH ROW EXECUTE FUNCTION reject_identity_deletion();
CREATE TRIGGER devices_no_delete BEFORE DELETE ON devices
    FOR EACH ROW EXECUTE FUNCTION reject_identity_deletion();
CREATE TRIGGER clients_no_delete BEFORE DELETE ON clients
    FOR EACH ROW EXECUTE FUNCTION reject_identity_deletion();
CREATE TRIGGER agents_no_delete BEFORE DELETE ON agents
    FOR EACH ROW EXECUTE FUNCTION reject_identity_deletion();
CREATE TRIGGER credentials_no_delete BEFORE DELETE ON credentials
    FOR EACH ROW EXECUTE FUNCTION reject_identity_deletion();

CREATE FUNCTION reject_ledger_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME;
END;
$$;

CREATE TRIGGER gateway_requests_append_only
    BEFORE UPDATE OR DELETE ON gateway_requests
    FOR EACH ROW EXECUTE FUNCTION reject_ledger_mutation();
CREATE TRIGGER provider_attempts_append_only
    BEFORE UPDATE OR DELETE ON provider_attempts
    FOR EACH ROW EXECUTE FUNCTION reject_ledger_mutation();
CREATE TRIGGER usage_records_append_only
    BEFORE UPDATE OR DELETE ON usage_records
    FOR EACH ROW EXECUTE FUNCTION reject_ledger_mutation();
CREATE TRIGGER audit_logs_append_only
    BEFORE UPDATE OR DELETE ON audit_logs
    FOR EACH ROW EXECUTE FUNCTION reject_ledger_mutation();

CREATE TRIGGER gateway_requests_no_truncate
    BEFORE TRUNCATE ON gateway_requests
    FOR EACH STATEMENT EXECUTE FUNCTION reject_ledger_mutation();
CREATE TRIGGER provider_attempts_no_truncate
    BEFORE TRUNCATE ON provider_attempts
    FOR EACH STATEMENT EXECUTE FUNCTION reject_ledger_mutation();
CREATE TRIGGER usage_records_no_truncate
    BEFORE TRUNCATE ON usage_records
    FOR EACH STATEMENT EXECUTE FUNCTION reject_ledger_mutation();
CREATE TRIGGER audit_logs_no_truncate
    BEFORE TRUNCATE ON audit_logs
    FOR EACH STATEMENT EXECUTE FUNCTION reject_ledger_mutation();

CREATE FUNCTION ingest_usage_envelopes(payload JSONB) RETURNS VOID
LANGUAGE plpgsql AS $$
BEGIN
    IF jsonb_typeof(payload) <> 'array' THEN
        RAISE EXCEPTION 'usage envelope payload must be an array';
    END IF;

    INSERT INTO gateway_requests (
        id, tenant_id, request_id, user_id, device_id, client_id, agent_id,
        credential_id, session_id, trace_id, workload_class, urgency, quality,
        budget_class, requested_model, selected_model, selected_provider,
        estimated_tokens, queue_duration, status, created_at, started_at, completed_at
    )
    SELECT
        r.id, r.tenant_id, r.request_id, r.user_id, r.device_id, r.client_id,
        r.agent_id, r.credential_id, r.session_id, r.trace_id, r.workload_class,
        r.urgency, r.quality, r.budget_class, r.requested_model,
        r.selected_model, r.selected_provider, r.estimated_tokens,
        r.queue_duration_ms * INTERVAL '1 millisecond', r.status,
        r.created_at, r.started_at, r.completed_at
    FROM jsonb_array_elements(payload) AS envelope
    CROSS JOIN LATERAL jsonb_to_record(envelope->'request') AS r(
        id UUID, tenant_id UUID, request_id TEXT, user_id UUID, device_id UUID,
        client_id UUID, agent_id UUID, credential_id UUID, session_id TEXT,
        trace_id TEXT, workload_class TEXT, urgency TEXT, quality TEXT,
        budget_class TEXT, requested_model TEXT, selected_model TEXT,
        selected_provider TEXT, estimated_tokens BIGINT, queue_duration_ms BIGINT,
        status TEXT, created_at TIMESTAMPTZ, started_at TIMESTAMPTZ,
        completed_at TIMESTAMPTZ
    )
    ON CONFLICT DO NOTHING;

    INSERT INTO provider_attempts (
        id, tenant_id, gateway_request_id, provider, model,
        provider_request_id, provider_response_id, attempt_number, started_at,
        first_token_at, completed_at, status, error_type,
        estimated_usage, actual_usage
    )
    SELECT
        a.id, a.tenant_id, a.gateway_request_id, a.provider, a.model,
        a.provider_request_id, a.provider_response_id, a.attempt_number,
        a.started_at, a.first_token_at, a.completed_at, a.status, a.error_type,
        a.estimated_usage, a.actual_usage
    FROM jsonb_array_elements(payload) AS envelope
    CROSS JOIN LATERAL jsonb_array_elements(envelope->'attempts') AS attempt
    CROSS JOIN LATERAL jsonb_to_record(attempt) AS a(
        id UUID, tenant_id UUID, gateway_request_id UUID, provider TEXT,
        model TEXT, provider_request_id TEXT, provider_response_id TEXT,
        attempt_number INTEGER, started_at TIMESTAMPTZ,
        first_token_at TIMESTAMPTZ, completed_at TIMESTAMPTZ, status TEXT,
        error_type TEXT, estimated_usage JSONB, actual_usage JSONB
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
