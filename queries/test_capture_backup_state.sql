BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
SELECT jsonb_build_object(
    'gateway_requests', COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)) FROM gateway_requests t), '[]'::jsonb),
    'provider_attempts', COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)) FROM provider_attempts t), '[]'::jsonb),
    'usage_records', COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)) FROM usage_records t), '[]'::jsonb),
    'request_parameter_snapshots', COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)) FROM request_parameter_snapshots t), '[]'::jsonb),
    'request_attribution', COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)) FROM request_attribution t), '[]'::jsonb),
    'quotas', COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)) FROM quotas t), '[]'::jsonb),
    'quota_windows', COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)) FROM quota_windows t), '[]'::jsonb),
    'quota_reservations', COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)) FROM quota_reservations t), '[]'::jsonb),
    'quota_reconciliation_events', COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)) FROM quota_reconciliation_events t), '[]'::jsonb),
    'capture_records', COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)) FROM capture_records t), '[]'::jsonb),
    'capture_blocks', COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)) FROM capture_blocks t), '[]'::jsonb)
);
ROLLBACK;
