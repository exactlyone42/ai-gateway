SELECT (state = 'deleted' AND wrapped_key IS NULL) AS "deleted!"
FROM capture_records WHERE tenant_id = $1 AND capture_id = $2
