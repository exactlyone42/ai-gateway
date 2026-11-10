INSERT INTO users (id, tenant_id, display_name, email) VALUES ($1, $2, $3, $4) RETURNING to_jsonb(users) AS "value!"
