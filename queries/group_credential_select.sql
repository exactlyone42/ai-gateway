SELECT c.group_id, c.digest
FROM group_provider_credentials c
JOIN group_members gm ON gm.tenant_id = c.tenant_id AND gm.group_id = c.group_id
JOIN users u ON u.tenant_id = gm.tenant_id AND u.id = gm.user_id
WHERE c.tenant_id = $1 AND gm.user_id = $2 AND c.provider_key = $3
  AND c.credential_kind = $4 AND c.enabled AND u.status = 'active'
ORDER BY c.group_id
LIMIT 2
FOR SHARE OF c, gm, u;
