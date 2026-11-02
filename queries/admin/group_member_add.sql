INSERT INTO group_members (tenant_id, group_id, user_id)
SELECT $1, $2, $3 WHERE EXISTS (SELECT 1 FROM groups WHERE tenant_id = $1 AND id = $2)
AND EXISTS (SELECT 1 FROM users WHERE tenant_id = $1 AND id = $3)
RETURNING user_id
