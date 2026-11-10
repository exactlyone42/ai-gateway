SELECT json_build_object(
    'attempts', (SELECT count(*) FROM provider_attempts),
    'completed_owner', (SELECT count(*) FROM gateway_requests WHERE user_id = :'owner'::uuid AND status = 'completed'),
    'other_attempts', (SELECT count(*) FROM provider_attempts a JOIN gateway_requests r ON r.id = a.gateway_request_id WHERE r.user_id IS DISTINCT FROM :'owner'::uuid),
    'reported_records', (SELECT count(*) FROM usage_records WHERE source = 'provider_reported'),
    'reported_input', (SELECT COALESCE(sum(input_tokens + cached_input_tokens + cache_write_input_tokens), 0) FROM usage_records WHERE source = 'provider_reported'),
    'reported_output', (SELECT COALESCE(sum(output_tokens + reasoning_tokens), 0) FROM usage_records WHERE source = 'provider_reported'),
    'models', (SELECT json_agg(model ORDER BY model) FROM provider_attempts),
    'providers', (SELECT json_agg(provider ORDER BY provider) FROM provider_attempts)
);
