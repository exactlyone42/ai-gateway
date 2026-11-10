SELECT 1 AS "present!" FROM capture_records WHERE tenant_id = $1 AND capture_id = $2 AND (state = 'deleting' OR (expires_at <= now() AND state <> 'deleted')) FOR UPDATE SKIP LOCKED
