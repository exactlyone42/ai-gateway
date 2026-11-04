SELECT
    (SELECT count(*) FROM gateway_requests WHERE request_id = :'request'),
    (SELECT count(*) FROM provider_attempts WHERE gateway_request_id IN (SELECT id FROM gateway_requests WHERE request_id = :'request')),
    (SELECT count(*) FROM usage_records WHERE gateway_request_id IN (SELECT id FROM gateway_requests WHERE request_id = :'request'));
