ALTER TABLE users
    ADD COLUMN status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'disabled')),
    ADD COLUMN version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();
ALTER TABLE devices
    ADD COLUMN status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'disabled')),
    ADD COLUMN version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();
ALTER TABLE clients
    ADD COLUMN status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'disabled')),
    ADD COLUMN version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();
ALTER TABLE agents
    ADD COLUMN status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'disabled')),
    ADD COLUMN version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();
ALTER TABLE credentials
    ADD COLUMN version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

CREATE TABLE admin_accounts (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    display_name TEXT NOT NULL CHECK (btrim(display_name) <> ''),
    role TEXT NOT NULL CHECK (role IN ('admin', 'operator', 'viewer')),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id)
);

CREATE INDEX admin_accounts_tenant_status_idx
    ON admin_accounts (tenant_id, status, id);

CREATE TABLE admin_bootstrap_claims (
    tenant_id UUID PRIMARY KEY,
    admin_account_id UUID NOT NULL,
    consumed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, admin_account_id),
    FOREIGN KEY (tenant_id, admin_account_id)
        REFERENCES admin_accounts (tenant_id, id)
);

CREATE TABLE admin_login_credentials (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    admin_account_id UUID NOT NULL,
    display_name TEXT NOT NULL CHECK (btrim(display_name) <> ''),
    secret_hash BYTEA NOT NULL CHECK (octet_length(secret_hash) = 32),
    hash_key_id TEXT NOT NULL CHECK (btrim(hash_key_id) <> ''),
    key_prefix TEXT NOT NULL CHECK (btrim(key_prefix) <> ''),
    fingerprint BYTEA NOT NULL CHECK (octet_length(fingerprint) = 32),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'revoked', 'expired')),
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, fingerprint),
    UNIQUE (tenant_id, id, admin_account_id),
    FOREIGN KEY (tenant_id, admin_account_id)
        REFERENCES admin_accounts (tenant_id, id),
    CHECK ((status = 'revoked') = (revoked_at IS NOT NULL)),
    CHECK (expires_at IS NULL OR expires_at > created_at)
);

CREATE INDEX admin_login_credentials_active_idx
    ON admin_login_credentials (tenant_id, fingerprint)
    WHERE status = 'active' AND revoked_at IS NULL;

CREATE TABLE admin_sessions (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    admin_account_id UUID NOT NULL,
    login_credential_id UUID NOT NULL,
    token_hash BYTEA NOT NULL CHECK (octet_length(token_hash) = 32),
    hash_key_id TEXT NOT NULL CHECK (btrim(hash_key_id) <> ''),
    fingerprint BYTEA NOT NULL CHECK (octet_length(fingerprint) = 32),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    source_ip INET NOT NULL,
    account_version BIGINT NOT NULL CHECK (account_version > 0),
    login_credential_version BIGINT NOT NULL CHECK (login_credential_version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, fingerprint),
    CONSTRAINT admin_sessions_account_fk FOREIGN KEY (tenant_id, admin_account_id)
        REFERENCES admin_accounts (tenant_id, id),
    CONSTRAINT admin_sessions_credential_owner_fk
        FOREIGN KEY (tenant_id, login_credential_id, admin_account_id)
        REFERENCES admin_login_credentials (tenant_id, id, admin_account_id),
    CHECK (expires_at > created_at),
    CHECK (revoked_at IS NULL OR revoked_at >= created_at)
);

CREATE INDEX admin_sessions_active_idx
    ON admin_sessions (tenant_id, fingerprint, expires_at)
    WHERE revoked_at IS NULL;

CREATE TABLE providers (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    provider_key TEXT NOT NULL CHECK (provider_key ~ '^[a-z][a-z0-9-]{0,62}$'),
    display_name TEXT NOT NULL CHECK (btrim(display_name) <> ''),
    provider_type TEXT NOT NULL CHECK (provider_type IN ('anthropic', 'openai', 'gemini', 'xai', 'local')),
    base_url TEXT NOT NULL CHECK (base_url ~ '^https?://'),
    credential_ref TEXT NOT NULL CHECK (credential_ref ~ '^env://[A-Z][A-Z0-9_]{2,127}$'),
    lifecycle_state TEXT NOT NULL DEFAULT 'draft'
        CHECK (lifecycle_state IN ('draft', 'archived')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, provider_key)
);

CREATE TABLE models (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    provider_id UUID NOT NULL,
    public_model_id TEXT NOT NULL CHECK (btrim(public_model_id) <> ''),
    provider_model_id TEXT NOT NULL CHECK (btrim(provider_model_id) <> ''),
    quality_tier TEXT CHECK (quality_tier IN ('economy', 'standard', 'advanced', 'premium')),
    context_window BIGINT CHECK (context_window IS NULL OR context_window > 0),
    max_output_tokens BIGINT CHECK (max_output_tokens IS NULL OR max_output_tokens > 0),
    capabilities JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(capabilities) = 'array'),
    supports_streaming BOOLEAN NOT NULL DEFAULT false,
    supports_tools BOOLEAN NOT NULL DEFAULT false,
    supports_reasoning BOOLEAN NOT NULL DEFAULT false,
    supports_images BOOLEAN NOT NULL DEFAULT false,
    supports_structured_output BOOLEAN NOT NULL DEFAULT false,
    cost_metadata JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(cost_metadata) = 'object'),
    latency_class TEXT,
    lifecycle_state TEXT NOT NULL DEFAULT 'draft'
        CHECK (lifecycle_state IN ('draft', 'archived')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, public_model_id),
    FOREIGN KEY (tenant_id, provider_id) REFERENCES providers (tenant_id, id)
);

CREATE TABLE policies (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    policy_key TEXT NOT NULL CHECK (btrim(policy_key) <> ''),
    revision BIGINT NOT NULL CHECK (revision > 0),
    user_id UUID,
    device_id UUID,
    client_id UUID,
    agent_id UUID,
    credential_id UUID,
    configuration JSONB NOT NULL CHECK (jsonb_typeof(configuration) = 'object'),
    lifecycle_state TEXT NOT NULL DEFAULT 'draft'
        CHECK (lifecycle_state IN ('draft', 'archived')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, policy_key, revision),
    FOREIGN KEY (tenant_id, user_id) REFERENCES users (tenant_id, id),
    FOREIGN KEY (tenant_id, device_id) REFERENCES devices (tenant_id, id),
    FOREIGN KEY (tenant_id, client_id) REFERENCES clients (tenant_id, id),
    FOREIGN KEY (tenant_id, agent_id) REFERENCES agents (tenant_id, id),
    FOREIGN KEY (tenant_id, credential_id) REFERENCES credentials (tenant_id, id),
    CHECK (num_nonnulls(user_id, device_id, client_id, agent_id, credential_id) <= 1)
);

CREATE TABLE quotas (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    quota_key TEXT NOT NULL CHECK (btrim(quota_key) <> ''),
    revision BIGINT NOT NULL CHECK (revision > 0),
    user_id UUID,
    device_id UUID,
    client_id UUID,
    agent_id UUID,
    credential_id UUID,
    model_id UUID,
    provider_id UUID,
    configuration JSONB NOT NULL CHECK (jsonb_typeof(configuration) = 'object'),
    lifecycle_state TEXT NOT NULL DEFAULT 'draft'
        CHECK (lifecycle_state IN ('draft', 'archived')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, quota_key, revision),
    FOREIGN KEY (tenant_id, user_id) REFERENCES users (tenant_id, id),
    FOREIGN KEY (tenant_id, device_id) REFERENCES devices (tenant_id, id),
    FOREIGN KEY (tenant_id, client_id) REFERENCES clients (tenant_id, id),
    FOREIGN KEY (tenant_id, agent_id) REFERENCES agents (tenant_id, id),
    FOREIGN KEY (tenant_id, credential_id) REFERENCES credentials (tenant_id, id),
    FOREIGN KEY (tenant_id, model_id) REFERENCES models (tenant_id, id),
    FOREIGN KEY (tenant_id, provider_id) REFERENCES providers (tenant_id, id),
    CHECK (num_nonnulls(user_id, device_id, client_id, agent_id, credential_id, model_id, provider_id) <= 1)
);

ALTER TABLE audit_logs
    ADD COLUMN actor_admin_id UUID,
    ADD COLUMN actor_role TEXT CHECK (actor_role IN ('admin', 'operator', 'viewer')),
    ADD COLUMN source_ip INET,
    ADD COLUMN peer_ip INET,
    ADD COLUMN request_id TEXT,
    ADD CHECK (request_id IS NULL OR btrim(request_id) <> ''),
    ADD CHECK ((actor_admin_id IS NULL) = (actor_role IS NULL));

ALTER TABLE audit_logs
    ADD CONSTRAINT audit_logs_admin_actor_fk
    FOREIGN KEY (tenant_id, actor_admin_id)
    REFERENCES admin_accounts (tenant_id, id) NOT VALID;

CREATE TRIGGER admin_bootstrap_claims_append_only
    BEFORE UPDATE OR DELETE ON admin_bootstrap_claims
    FOR EACH ROW EXECUTE FUNCTION reject_ledger_mutation();
CREATE TRIGGER admin_bootstrap_claims_no_truncate
    BEFORE TRUNCATE ON admin_bootstrap_claims
    FOR EACH STATEMENT EXECUTE FUNCTION reject_ledger_mutation();
