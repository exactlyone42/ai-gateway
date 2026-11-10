UPDATE group_provider_credentials SET enabled = false
WHERE tenant_id = $1 AND group_id = $2 AND provider_key = $3 AND credential_kind = $4;
