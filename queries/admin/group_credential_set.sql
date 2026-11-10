INSERT INTO group_provider_credentials (tenant_id, group_id, provider_key, credential_kind, digest, enabled)
VALUES ($1, $2, $3, $4, $5, true)
ON CONFLICT (tenant_id, group_id, provider_key, credential_kind)
DO UPDATE SET digest = EXCLUDED.digest, enabled = true;
