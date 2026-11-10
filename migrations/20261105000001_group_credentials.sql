CREATE TABLE group_provider_credentials (
    tenant_id UUID NOT NULL,
    group_id UUID NOT NULL,
    provider_key TEXT NOT NULL,
    credential_kind TEXT NOT NULL CHECK (credential_kind IN ('subscription', 'api')),
    digest BYTEA NOT NULL CHECK (octet_length(digest) = 32),
    enabled BOOLEAN NOT NULL DEFAULT true,
    PRIMARY KEY (tenant_id, group_id, provider_key, credential_kind),
    FOREIGN KEY (tenant_id, group_id) REFERENCES groups (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, provider_key) REFERENCES providers (tenant_id, provider_key) ON DELETE RESTRICT
);
