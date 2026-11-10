SELECT g.id
FROM groups g
JOIN providers p ON p.tenant_id = g.tenant_id AND p.provider_key = $3
WHERE g.tenant_id = $1 AND g.id = $2 AND p.lifecycle_state = 'active'
  AND ($4 = 'api' OR p.provider_type = 'anthropic')
FOR UPDATE OF g
FOR SHARE OF p;
