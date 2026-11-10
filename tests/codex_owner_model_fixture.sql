WITH provider AS (
    INSERT INTO providers (id, tenant_id, provider_key, display_name, provider_type, base_url, credential_ref, supported_operations, lifecycle_state)
    VALUES ($1, $2, 'codex-subscription', 'Codex fixture', 'openai', $3, 'env://CODEX_AUTH_FILE', ARRAY['openai_responses'], 'active')
    RETURNING id
)
INSERT INTO models (id, tenant_id, provider_id, public_model_id, provider_model_id, display_name, supported_operations, lifecycle_state)
SELECT $4, $2, id, 'owner-model', 'owner-model', 'Owner fixture', ARRAY['openai_responses'], 'active' FROM provider
