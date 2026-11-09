UPDATE capture_records SET state = $3 WHERE tenant_id = $1 AND capture_id = $2 AND state IS DISTINCT FROM $3
