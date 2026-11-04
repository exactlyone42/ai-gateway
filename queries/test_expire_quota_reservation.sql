UPDATE quota_reservations SET state = 'expired' WHERE tenant_id = $1 AND id = $2 AND state = 'active'
