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
    workload_class TEXT NOT NULL,
    urgency TEXT NOT NULL,
    quality TEXT NOT NULL,
    budget_class TEXT NOT NULL,
    requested_model TEXT NOT NULL,
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
