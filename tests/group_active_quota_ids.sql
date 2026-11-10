SELECT id FROM quotas WHERE tenant_id = $1 AND lifecycle_state = 'active' ORDER BY id
