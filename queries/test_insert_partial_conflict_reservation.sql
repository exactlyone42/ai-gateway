INSERT INTO quota_reservations (id, tenant_id, gateway_request_id, state, estimated_tokens, estimated_cost_microusd, decision, allocations, expires_at)
VALUES ($1, $2, $3, 'active', 0, 0, '{}'::jsonb, '[]'::jsonb, now() + interval '1 day')
