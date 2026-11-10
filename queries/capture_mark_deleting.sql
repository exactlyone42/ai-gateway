UPDATE capture_records SET state = 'deleting' WHERE tenant_id = $1 AND capture_id = $2
