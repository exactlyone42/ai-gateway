-- W04 acceptance queries. Every statement is tenant-scoped and can be loaded
-- directly by psql. Execute the named statement with the documented arguments.

-- 1. Who used tokens? tenant_id, user_id.
PREPARE usage_by_user(UUID, UUID) AS
SELECT ur.* FROM usage_records ur
JOIN gateway_requests gr ON (gr.tenant_id, gr.id) = (ur.tenant_id, ur.gateway_request_id)
WHERE ur.tenant_id = $1 AND gr.user_id = $2 ORDER BY ur.recorded_at;

-- 2. Which device used tokens? tenant_id, device_id.
PREPARE usage_by_device(UUID, UUID) AS
SELECT ur.* FROM usage_records ur
JOIN gateway_requests gr ON (gr.tenant_id, gr.id) = (ur.tenant_id, ur.gateway_request_id)
WHERE ur.tenant_id = $1 AND gr.device_id = $2 ORDER BY ur.recorded_at;

-- 3. Which client used tokens? tenant_id, client_id.
PREPARE usage_by_client(UUID, UUID) AS
SELECT ur.* FROM usage_records ur
JOIN gateway_requests gr ON (gr.tenant_id, gr.id) = (ur.tenant_id, ur.gateway_request_id)
WHERE ur.tenant_id = $1 AND gr.client_id = $2 ORDER BY ur.recorded_at;

-- 4. Which agent used tokens? tenant_id, agent_id.
PREPARE usage_by_agent(UUID, UUID) AS
SELECT ur.* FROM usage_records ur
JOIN gateway_requests gr ON (gr.tenant_id, gr.id) = (ur.tenant_id, ur.gateway_request_id)
WHERE ur.tenant_id = $1 AND gr.agent_id = $2 ORDER BY ur.recorded_at;

-- 5. Which credential used tokens? tenant_id, credential_id.
PREPARE usage_by_credential(UUID, UUID) AS
SELECT ur.* FROM usage_records ur
JOIN gateway_requests gr ON (gr.tenant_id, gr.id) = (ur.tenant_id, ur.gateway_request_id)
WHERE ur.tenant_id = $1 AND gr.credential_id = $2 ORDER BY ur.recorded_at;

-- 6. Which attempted model was billed? tenant_id, model.
PREPARE usage_by_model(UUID, TEXT) AS
SELECT ur.* FROM usage_records ur
JOIN provider_attempts pa
  ON (pa.tenant_id, pa.gateway_request_id, pa.id) =
     (ur.tenant_id, ur.gateway_request_id, ur.provider_attempt_id)
WHERE ur.tenant_id = $1 AND pa.model = $2 ORDER BY ur.recorded_at;

-- 7. Which attempted provider was billed? tenant_id, provider.
PREPARE usage_by_provider(UUID, TEXT) AS
SELECT ur.* FROM usage_records ur
JOIN provider_attempts pa
  ON (pa.tenant_id, pa.gateway_request_id, pa.id) =
     (ur.tenant_id, ur.gateway_request_id, ur.provider_attempt_id)
WHERE ur.tenant_id = $1 AND pa.provider = $2 ORDER BY ur.recorded_at;

-- 8. What was used in a UTC interval? tenant_id, inclusive start, exclusive end.
PREPARE usage_by_utc_interval(UUID, TIMESTAMPTZ, TIMESTAMPTZ) AS
SELECT ur.* FROM usage_records ur
WHERE ur.tenant_id = $1 AND ur.recorded_at >= $2 AND ur.recorded_at < $3
ORDER BY ur.recorded_at;

-- 9. Provider-reported totals, kept separate by provider and model because
-- token definitions are not interchangeable across providers.
PREPARE provider_reported_totals(UUID) AS
SELECT
    pa.provider,
    pa.model,
    COALESCE(sum(ur.input_tokens), 0) AS input_tokens,
    COALESCE(sum(ur.output_tokens), 0) AS output_tokens,
    COALESCE(sum(ur.cached_input_tokens), 0) AS cached_input_tokens,
    COALESCE(sum(ur.cache_write_input_tokens), 0) AS cache_write_input_tokens,
    COALESCE(sum(ur.reasoning_tokens), 0) AS reasoning_tokens,
    COALESCE(sum(ur.cost_microusd), 0) AS cost_microusd
FROM usage_records ur
JOIN provider_attempts pa
  ON (pa.tenant_id, pa.gateway_request_id, pa.id) =
     (ur.tenant_id, ur.gateway_request_id, ur.provider_attempt_id)
WHERE ur.tenant_id = $1 AND ur.source = 'provider_reported'
GROUP BY pa.provider, pa.model
ORDER BY pa.provider, pa.model;
