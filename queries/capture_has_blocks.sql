SELECT EXISTS (SELECT 1 FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2) AS "exists!"
