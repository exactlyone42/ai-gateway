SELECT EXISTS (
    SELECT 1 FROM group_provider_credentials
    WHERE tenant_id = $1 AND group_id = $2 AND enabled
) AS "bound!";
