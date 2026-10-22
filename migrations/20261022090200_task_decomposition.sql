-- Outcome of one shadow decomposition. `reason` is a fixed code (never model
-- text) and there is no column that could hold an objective or prompt.
CREATE TABLE task_decompositions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL CHECK (
        tenant_id <> '00000000-0000-0000-0000-000000000000'::uuid
    ),
    orchestration_id UUID NOT NULL,
    gateway_request_id UUID NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('direct', 'decomposed')),
    reason TEXT NOT NULL CHECK (reason ~ '^[a-z0-9_]{1,64}$'),
    node_count INTEGER NOT NULL CHECK (node_count BETWEEN 0 AND 8),
    max_depth INTEGER NOT NULL CHECK (max_depth BETWEEN 0 AND 2),
    elapsed_us BIGINT NOT NULL CHECK (elapsed_us >= 0),
    contract_version TEXT NOT NULL CHECK (contract_version ~ '^[a-z0-9.-]{1,32}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, orchestration_id),
    CHECK ((decision = 'direct') = (node_count = 0))
);

CREATE INDEX task_decompositions_recent_idx
    ON task_decompositions (tenant_id, created_at DESC);

CREATE TRIGGER task_decompositions_append_only
    BEFORE UPDATE OR DELETE ON task_decompositions
    FOR EACH ROW EXECUTE FUNCTION reject_ledger_mutation();
CREATE TRIGGER task_decompositions_no_truncate
    BEFORE TRUNCATE ON task_decompositions
    FOR EACH STATEMENT EXECUTE FUNCTION reject_ledger_mutation();
