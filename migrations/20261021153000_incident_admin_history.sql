CREATE TABLE incidents (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    title TEXT NOT NULL CHECK (btrim(title) <> '' AND octet_length(title) <= 512),
    status TEXT NOT NULL CHECK (status IN ('investigating', 'mitigated', 'resolved', 'closed')),
    service TEXT NOT NULL CHECK (btrim(service) <> '' AND octet_length(service) <= 128),
    impact_scope TEXT NOT NULL CHECK (impact_scope IN ('system', 'tenant', 'principal')),
    started_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ,
    summary TEXT NOT NULL CHECK (btrim(summary) <> '' AND octet_length(summary) <= 8192),
    impact TEXT NOT NULL CHECK (btrim(impact) <> '' AND octet_length(impact) <= 8192),
    root_cause TEXT CHECK (root_cause IS NULL OR (btrim(root_cause) <> '' AND octet_length(root_cause) <= 8192)),
    temporary_fix TEXT CHECK (temporary_fix IS NULL OR (btrim(temporary_fix) <> '' AND octet_length(temporary_fix) <= 8192)),
    permanent_fix TEXT CHECK (permanent_fix IS NULL OR (btrim(permanent_fix) <> '' AND octet_length(permanent_fix) <= 8192)),
    prevention TEXT CHECK (prevention IS NULL OR (btrim(prevention) <> '' AND octet_length(prevention) <= 8192)),
    validation_evidence TEXT CHECK (validation_evidence IS NULL OR (btrim(validation_evidence) <> '' AND octet_length(validation_evidence) <= 8192)),
    created_by UUID NOT NULL,
    updated_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, created_by) REFERENCES admin_accounts (tenant_id, id),
    FOREIGN KEY (tenant_id, updated_by) REFERENCES admin_accounts (tenant_id, id),
    CHECK (ended_at IS NULL OR ended_at >= started_at)
);

CREATE INDEX incidents_admin_filter_idx
    ON incidents (tenant_id, status, service, impact_scope, started_at DESC, id DESC);

CREATE TABLE incident_events (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    incident_id UUID NOT NULL,
    actor_admin_id UUID NOT NULL,
    event_type TEXT NOT NULL CHECK (event_type IN (
        'created', 'status_changed', 'timeline', 'analysis', 'remediation', 'verification', 'link'
    )),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    message TEXT CHECK (message IS NULL OR (btrim(message) <> '' AND octet_length(message) <= 8192)),
    before_state JSONB,
    after_state JSONB,
    gateway_request_id UUID,
    deployment_version TEXT CHECK (
        deployment_version IS NULL OR (btrim(deployment_version) <> '' AND octet_length(deployment_version) <= 255)
    ),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, incident_id) REFERENCES incidents (tenant_id, id),
    FOREIGN KEY (tenant_id, actor_admin_id) REFERENCES admin_accounts (tenant_id, id),
    FOREIGN KEY (tenant_id, gateway_request_id) REFERENCES gateway_requests (tenant_id, id),
    CHECK (num_nonnulls(message, before_state, after_state, gateway_request_id, deployment_version) > 0)
);

CREATE INDEX incident_events_timeline_idx
    ON incident_events (tenant_id, incident_id, occurred_at, id);

CREATE FUNCTION reject_incident_deletion() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is retained for incident history', TG_TABLE_NAME;
END;
$$;

CREATE TRIGGER incidents_no_delete
    BEFORE DELETE ON incidents
    FOR EACH ROW EXECUTE FUNCTION reject_incident_deletion();
CREATE TRIGGER incidents_no_truncate
    BEFORE TRUNCATE ON incidents
    FOR EACH STATEMENT EXECUTE FUNCTION reject_incident_deletion();
CREATE TRIGGER incident_events_append_only
    BEFORE UPDATE OR DELETE ON incident_events
    FOR EACH ROW EXECUTE FUNCTION reject_ledger_mutation();
CREATE TRIGGER incident_events_no_truncate
    BEFORE TRUNCATE ON incident_events
    FOR EACH STATEMENT EXECUTE FUNCTION reject_ledger_mutation();
