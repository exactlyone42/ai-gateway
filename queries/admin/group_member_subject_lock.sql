SELECT id FROM groups WHERE tenant_id = $1 AND id = $2 FOR SHARE;
