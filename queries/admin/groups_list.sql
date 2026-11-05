SELECT to_jsonb(g) AS "value!"
FROM groups g WHERE g.tenant_id = $1 AND g.id > $2 ORDER BY g.id LIMIT $3
