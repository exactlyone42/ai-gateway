SELECT COALESCE(json_agg(record ORDER BY created_at, id), '[]'::json)
FROM (
    SELECT r.id, r.request_id, a.id AS attempt_id, a.provider_request_id, a.error_type, r.status, r.created_at, a.actual_usage, a.estimated_usage,
        (SELECT json_agg(json_build_object('source', u.source, 'input_tokens', u.input_tokens, 'output_tokens', u.output_tokens, 'cached_input_tokens', u.cached_input_tokens, 'cache_write_input_tokens', u.cache_write_input_tokens, 'reasoning_tokens', u.reasoning_tokens))
         FROM usage_records u WHERE u.provider_attempt_id = a.id) AS usage
    FROM gateway_requests r
    JOIN provider_attempts a ON a.gateway_request_id = r.id
    WHERE r.user_id = :'user'::uuid
) AS record;
