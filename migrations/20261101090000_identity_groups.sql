ALTER TABLE users
    ADD COLUMN email TEXT
        CHECK (email IS NULL OR (btrim(email) <> '' AND char_length(email) <= 320));

CREATE UNIQUE INDEX users_tenant_email_idx
    ON users (tenant_id, lower(email))
    WHERE email IS NOT NULL;

CREATE TABLE groups (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    display_name TEXT NOT NULL CHECK (btrim(display_name) <> '' AND char_length(display_name) <= 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id)
);

CREATE UNIQUE INDEX groups_tenant_name_idx ON groups (tenant_id, lower(display_name));

CREATE TABLE group_members (
    tenant_id UUID NOT NULL,
    group_id UUID NOT NULL,
    user_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, group_id, user_id),
    FOREIGN KEY (tenant_id, group_id) REFERENCES groups (tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, user_id) REFERENCES users (tenant_id, id) ON DELETE RESTRICT
);

CREATE INDEX group_members_user_idx ON group_members (tenant_id, user_id);
