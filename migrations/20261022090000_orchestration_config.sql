-- Per-tenant switch for multi-model orchestration. A missing row means
-- disabled, so no tenant is observed until an operator opts in.
CREATE TABLE orchestration_configs (
    tenant_id UUID PRIMARY KEY CHECK (
        tenant_id <> '00000000-0000-0000-0000-000000000000'::uuid
    ),
    mode TEXT NOT NULL DEFAULT 'disabled' CHECK (mode IN ('disabled', 'shadow')),
    data_policy TEXT NOT NULL DEFAULT 'local_only' CHECK (
        data_policy IN ('local_only', 'online_allowed')
    ),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
