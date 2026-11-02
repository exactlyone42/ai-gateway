SELECT id FROM policies WHERE tenant_id = $1 AND lifecycle_state = 'active' ORDER BY id
