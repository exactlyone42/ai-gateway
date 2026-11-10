SELECT jsonb_build_object(
    'state', (SELECT state FROM quota_reservations WHERE tenant_id = $1 AND id = $2),
    'events', (SELECT count(*) FROM quota_reconciliation_events WHERE tenant_id = $1 AND reservation_id = $2),
    'original', (SELECT reserved_tokens FROM quota_windows WHERE tenant_id = $1 AND limit_kind = 'tokens_per_hour' AND window_start <= $3 AND window_end > $3),
    'later', (SELECT reserved_tokens FROM quota_windows WHERE tenant_id = $1 AND limit_kind = 'tokens_per_hour' AND window_start <= $4 AND window_end > $4)
) AS "evidence!"
