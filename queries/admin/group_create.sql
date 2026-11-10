INSERT INTO groups (id, tenant_id, display_name) VALUES ($1, $2, $3) RETURNING to_jsonb(groups) AS "value!"
