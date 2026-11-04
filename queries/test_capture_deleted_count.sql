SELECT count(*) AS "count!" FROM capture_records
WHERE tenant_id = $1 AND state = 'deleted' AND wrapped_key IS NULL;
