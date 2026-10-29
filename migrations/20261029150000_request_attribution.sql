ALTER FUNCTION ingest_usage_envelopes(JSONB)
    RENAME TO ingest_usage_envelopes_without_attribution;

CREATE FUNCTION valid_request_attribution(value TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT value IS NULL OR (octet_length(value) BETWEEN 1 AND 128 AND value ~ '^[ -~]+$');
$$;

CREATE TABLE request_attribution (
    tenant_id UUID NOT NULL,
    gateway_request_id UUID NOT NULL,
    prompt_id TEXT CHECK (valid_request_attribution(prompt_id)),
    prompt_version TEXT CHECK (valid_request_attribution(prompt_version)),
    skill_set_id TEXT CHECK (valid_request_attribution(skill_set_id)),
    agent_name TEXT CHECK (valid_request_attribution(agent_name)),
    agent_version TEXT CHECK (valid_request_attribution(agent_version)),
    client_version TEXT CHECK (valid_request_attribution(client_version)),
    task_id TEXT CHECK (valid_request_attribution(task_id)),
    PRIMARY KEY (tenant_id, gateway_request_id),
    FOREIGN KEY (tenant_id, gateway_request_id) REFERENCES gateway_requests (tenant_id, id)
);

CREATE INDEX request_attribution_task_idx
    ON request_attribution (tenant_id, task_id, gateway_request_id)
    WHERE task_id IS NOT NULL;

CREATE TRIGGER request_attribution_append_only
    BEFORE UPDATE OR DELETE ON request_attribution
    FOR EACH ROW EXECUTE FUNCTION reject_ledger_mutation();

CREATE TRIGGER request_attribution_no_truncate
    BEFORE TRUNCATE ON request_attribution
    FOR EACH STATEMENT EXECUTE FUNCTION reject_ledger_mutation();

CREATE FUNCTION ingest_usage_envelopes(payload JSONB) RETURNS VOID
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM ingest_usage_envelopes_without_attribution(payload);

    INSERT INTO request_attribution (
        tenant_id, gateway_request_id, prompt_id, prompt_version, skill_set_id,
        agent_name, agent_version, client_version, task_id
    )
    SELECT (envelope->'request'->>'tenant_id')::uuid,
           (envelope->'request'->>'id')::uuid,
           attrs->>'prompt_id', attrs->>'prompt_version', attrs->>'skill_set_id',
           attrs->>'agent_name', attrs->>'agent_version', attrs->>'client_version', attrs->>'task_id'
    FROM jsonb_array_elements(payload) AS envelope
    CROSS JOIN LATERAL (SELECT envelope->'request'->'attribution' AS attrs) AS selected
    WHERE jsonb_typeof(attrs) = 'object'
      AND attrs <> '{"prompt_id":null,"prompt_version":null,"skill_set_id":null,"agent_name":null,"agent_version":null,"client_version":null,"task_id":null}'::jsonb
    ON CONFLICT DO NOTHING;
END;
$$;
