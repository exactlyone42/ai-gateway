SELECT state, wrapped_key IS NULL AS "key_removed!", (SELECT count(*) FROM capture_blocks WHERE tenant_id = $1 AND capture_id = $2) AS "blocks!"
FROM capture_records WHERE tenant_id = $1 AND capture_id = $2
