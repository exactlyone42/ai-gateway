CREATE TABLE capture_records (
    tenant_id uuid NOT NULL,
    capture_id uuid NOT NULL,
    user_id uuid NOT NULL,
    key_fingerprint bytea NOT NULL CHECK (octet_length(key_fingerprint) = 32),
    wrapped_key bytea CHECK (octet_length(wrapped_key) = 60),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    state text NOT NULL CHECK (state IN ('recording', 'complete', 'incomplete', 'deleting', 'deleted')),
    PRIMARY KEY (tenant_id, capture_id),
    FOREIGN KEY (tenant_id, user_id) REFERENCES users (tenant_id, id),
    CHECK (expires_at > created_at AND expires_at <= created_at + interval '365 days'),
    CHECK ((state = 'deleted') = (wrapped_key IS NULL))
);

CREATE INDEX capture_records_expiry_idx ON capture_records (expires_at)
    WHERE state <> 'deleted';

CREATE TABLE capture_blocks (
    tenant_id uuid NOT NULL,
    capture_id uuid NOT NULL,
    leg smallint NOT NULL CHECK (leg BETWEEN 0 AND 4),
    sequence bigint NOT NULL CHECK (sequence BETWEEN 0 AND 999999),
    position bigint NOT NULL CHECK (position >= 0),
    elapsed_ns bigint NOT NULL CHECK (elapsed_ns >= 0),
    sealed bytea NOT NULL CHECK (octet_length(sealed) BETWEEN 28 AND 1048604),
    PRIMARY KEY (tenant_id, capture_id, leg, sequence),
    UNIQUE (tenant_id, capture_id, position),
    FOREIGN KEY (tenant_id, capture_id) REFERENCES capture_records (tenant_id, capture_id)
);
