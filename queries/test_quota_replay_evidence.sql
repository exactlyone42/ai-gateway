SELECT jsonb_build_object(
    'events', (SELECT count(*) FROM quota_reconciliation_events WHERE tenant_id = $1 AND reservation_id = $2),
    'state', (SELECT state FROM quota_reservations WHERE tenant_id = $1 AND id = $2),
    'actual_tokens', (SELECT actual_tokens FROM quota_reservations WHERE tenant_id = $1 AND id = $2),
    'window_tokens', (SELECT reserved_tokens FROM quota_windows WHERE tenant_id = $1 AND limit_kind = 'tokens_per_hour')
) AS "evidence!"
