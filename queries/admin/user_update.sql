UPDATE users SET display_name = COALESCE($4, display_name), status = COALESCE($5, status),
    email = CASE WHEN $6 THEN $7 ELSE email END, version = version + 1, updated_at = now()
WHERE tenant_id = $1 AND id = $2 AND version = $3 RETURNING to_jsonb(users) AS "value!"
