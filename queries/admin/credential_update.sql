UPDATE credentials
SET display_name = COALESCE($4, display_name),
    expires_at = COALESCE($5, expires_at), status = COALESCE($6, status),
    version = version + 1, updated_at = now()
WHERE tenant_id = $1 AND id = $2 AND version = $3
  AND status IN ('active', 'disabled') AND revoked_at IS NULL
  AND ($6::text IS NULL OR COALESCE($5, expires_at) IS NULL OR COALESCE($5, expires_at) > now())
RETURNING (to_jsonb(credentials) - ARRAY['secret_hash', 'fingerprint']::text[]) AS "value!"
