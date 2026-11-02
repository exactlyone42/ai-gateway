SELECT to_jsonb(gm) AS "value!" FROM group_members gm
WHERE gm.tenant_id = $1 AND gm.group_id = $2 AND gm.user_id > $3
ORDER BY gm.user_id LIMIT $4
