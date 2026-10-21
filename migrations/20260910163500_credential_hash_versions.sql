ALTER TABLE credentials
    ADD COLUMN hash_key_id TEXT NOT NULL DEFAULT 'v1'
        CHECK (btrim(hash_key_id) <> '');

ALTER TABLE credentials
    ALTER COLUMN hash_key_id DROP DEFAULT;

CREATE UNIQUE INDEX credentials_one_successor
    ON credentials (tenant_id, rotated_from_id)
    WHERE rotated_from_id IS NOT NULL;
