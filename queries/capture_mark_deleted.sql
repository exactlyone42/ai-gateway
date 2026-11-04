UPDATE capture_records SET state = 'deleted', wrapped_key = NULL WHERE tenant_id = $1 AND capture_id = $2
