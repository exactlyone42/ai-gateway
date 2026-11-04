SELECT tenant_id, capture_id FROM capture_records WHERE state = 'deleting' OR (expires_at <= now() AND state <> 'deleted') ORDER BY expires_at, tenant_id, capture_id FOR UPDATE SKIP LOCKED LIMIT 1
