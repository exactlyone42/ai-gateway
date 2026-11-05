SELECT EXISTS (
    SELECT 1 FROM credentials c
    WHERE c.tenant_id = $1 AND c.id = $2 AND c.status = 'active'
      AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at > now())
      AND (c.user_id IS NULL OR EXISTS (SELECT 1 FROM users u WHERE u.tenant_id = c.tenant_id AND u.id = c.user_id AND u.status = 'active'))
      AND (c.device_id IS NULL OR EXISTS (SELECT 1 FROM devices d WHERE d.tenant_id = c.tenant_id AND d.id = c.device_id AND d.status = 'active'))
      AND (c.client_id IS NULL OR EXISTS (SELECT 1 FROM clients cl WHERE cl.tenant_id = c.tenant_id AND cl.id = c.client_id AND cl.status = 'active'))
      AND (c.agent_id IS NULL OR EXISTS (SELECT 1 FROM agents a WHERE a.tenant_id = c.tenant_id AND a.id = c.agent_id AND a.status = 'active'))
) AS "active!"
