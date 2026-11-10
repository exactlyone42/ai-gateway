WITH p AS (
    UPDATE providers SET base_url = $1 WHERE tenant_id = $2 AND provider_key = 'anthropic-test'
    RETURNING id
)
INSERT INTO models (id, tenant_id, provider_id, public_model_id, provider_model_id, display_name, supported_operations, lifecycle_state)
SELECT '00000000-0000-0000-0000-000000001001', $2, id, 'group-model', 'upstream-model', 'group', ARRAY['anthropic_messages'], 'active' FROM p;
