CREATE TABLE users (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    display_name TEXT NOT NULL CHECK (btrim(display_name) <> ''),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id)
);

CREATE TABLE devices (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    display_name TEXT NOT NULL CHECK (btrim(display_name) <> ''),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id)
);

CREATE TABLE clients (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    display_name TEXT NOT NULL CHECK (btrim(display_name) <> ''),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id)
);

CREATE TABLE agents (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    display_name TEXT NOT NULL CHECK (btrim(display_name) <> ''),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id)
);

CREATE TABLE credentials (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    "type" TEXT NOT NULL CHECK ("type" IN ('client_api_key')),
    display_name TEXT NOT NULL CHECK (btrim(display_name) <> ''),
    secret_hash BYTEA NOT NULL CHECK (octet_length(secret_hash) = 32),
    key_prefix TEXT NOT NULL,
    fingerprint BYTEA NOT NULL CHECK (octet_length(fingerprint) = 32),
    user_id UUID,
    device_id UUID,
    client_id UUID,
    agent_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'disabled', 'revoked', 'expired')),
    rotated_from_id UUID,
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, fingerprint),
    FOREIGN KEY (tenant_id, user_id) REFERENCES users (tenant_id, id),
    FOREIGN KEY (tenant_id, device_id) REFERENCES devices (tenant_id, id),
    FOREIGN KEY (tenant_id, client_id) REFERENCES clients (tenant_id, id),
    FOREIGN KEY (tenant_id, agent_id) REFERENCES agents (tenant_id, id),
    FOREIGN KEY (tenant_id, rotated_from_id) REFERENCES credentials (tenant_id, id),
    CHECK (num_nonnulls(user_id, device_id, client_id, agent_id) > 0),
    CHECK ((status = 'revoked') = (revoked_at IS NOT NULL)),
    CHECK (expires_at IS NULL OR expires_at > created_at),
    CHECK (last_used_at IS NULL OR last_used_at >= created_at),
    CHECK (revoked_at IS NULL OR revoked_at >= created_at)
);

CREATE INDEX credentials_active_fingerprint_idx
    ON credentials (tenant_id, fingerprint)
    WHERE status = 'active' AND revoked_at IS NULL;
