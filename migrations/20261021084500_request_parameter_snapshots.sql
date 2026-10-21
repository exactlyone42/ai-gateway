ALTER FUNCTION ingest_usage_envelopes(JSONB)
    RENAME TO ingest_usage_envelopes_without_parameters;

CREATE FUNCTION valid_request_parameter_snapshot(parameters JSONB) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT jsonb_typeof(parameters) = 'object'
       AND octet_length(parameters::text) <= 4096
       AND parameters - ARRAY[
           'schema_version', 'operation', 'model', 'max_output_tokens',
           'temperature', 'top_p', 'top_k', 'stream', 'parallel_tool_calls',
           'message_count', 'content_part_count', 'text_chars', 'image_count',
           'tool_count', 'has_system', 'has_structured_output', 'reasoning'
       ] = '{}'::jsonb;
$$;

CREATE TABLE request_parameter_snapshots (
    tenant_id UUID NOT NULL,
    gateway_request_id UUID NOT NULL,
    stage TEXT NOT NULL CHECK (stage IN ('requested', 'provider')),
    parameters JSONB NOT NULL CHECK (valid_request_parameter_snapshot(parameters)),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, gateway_request_id, stage),
    FOREIGN KEY (tenant_id, gateway_request_id)
        REFERENCES gateway_requests (tenant_id, id)
);

CREATE TRIGGER request_parameter_snapshots_append_only
    BEFORE UPDATE OR DELETE ON request_parameter_snapshots
    FOR EACH ROW EXECUTE FUNCTION reject_ledger_mutation();

CREATE TRIGGER request_parameter_snapshots_no_truncate
    BEFORE TRUNCATE ON request_parameter_snapshots
    FOR EACH STATEMENT EXECUTE FUNCTION reject_ledger_mutation();

CREATE FUNCTION ingest_usage_envelopes(payload JSONB) RETURNS VOID
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM ingest_usage_envelopes_without_parameters(payload);

    INSERT INTO request_parameter_snapshots (
        tenant_id, gateway_request_id, stage, parameters, recorded_at
    )
    SELECT
        (envelope->'request'->>'tenant_id')::uuid,
        (envelope->'request'->>'id')::uuid,
        snapshot.stage,
        snapshot.parameters,
        COALESCE((envelope->'request'->>'created_at')::timestamptz, now())
    FROM jsonb_array_elements(payload) AS envelope
    CROSS JOIN LATERAL (VALUES
        ('requested', envelope->'request'->'requested_parameters'),
        ('provider', envelope->'request'->'provider_parameters')
    ) AS snapshot(stage, parameters)
    WHERE snapshot.parameters IS NOT NULL
      AND jsonb_typeof(snapshot.parameters) = 'object'
    ON CONFLICT DO NOTHING;
END;
$$;
