-- One receipt per model-backed step of an orchestration (decomposer,
-- worker, integrator). Metadata only: no prompt, objective or output.
CREATE TABLE orchestration_executions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL CHECK (
        tenant_id <> '00000000-0000-0000-0000-000000000000'::uuid
    ),
    orchestration_id UUID NOT NULL,
    execution_role TEXT NOT NULL CHECK (
        execution_role IN ('decomposer', 'worker', 'integrator')
    ),
    task_id TEXT NOT NULL CHECK (task_id ~ '^[A-Za-z0-9_-]{1,64}$'),
    attempt INTEGER NOT NULL DEFAULT 1 CHECK (attempt > 0),
    status TEXT NOT NULL CHECK (status IN ('completed', 'failed', 'skipped')),
    elapsed_us BIGINT NOT NULL CHECK (elapsed_us >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, orchestration_id, execution_role, task_id, attempt)
);

CREATE INDEX orchestration_executions_recent_idx
    ON orchestration_executions (tenant_id, created_at DESC);
