WITH requests AS (
    SELECT id, status, completed_at
    FROM gateway_requests
    WHERE user_id = :'user'::uuid
)
SELECT json_build_object(
    'requests', (SELECT count(*) FROM requests),
    'cancelled', (SELECT count(*) FROM requests WHERE status = 'cancelled' AND completed_at IS NOT NULL),
    'attempts', (SELECT count(*) FROM provider_attempts WHERE gateway_request_id IN (SELECT id FROM requests)),
    'usage_records', (SELECT count(*) FROM usage_records WHERE gateway_request_id IN (SELECT id FROM requests))
);
