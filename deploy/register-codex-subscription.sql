BEGIN;
INSERT INTO providers (
    id, tenant_id, provider_key, display_name, provider_type, base_url,
    credential_ref, supported_operations, lifecycle_state
)
VALUES (
    :'provider_id', :'tenant_id', 'codex-subscription', 'Codex 訂閱', 'openai',
    'https://chatgpt.com/backend-api/codex', 'env://CODEX_AUTH_FILE',
    ARRAY['openai_responses'], 'active'
);
INSERT INTO models (
    id, tenant_id, provider_id, public_model_id, provider_model_id, display_name,
    supported_operations, supports_streaming, supports_tools, supports_reasoning,
    supports_images, supports_structured_output, lifecycle_state
)
VALUES (
    :'model_id', :'tenant_id', :'provider_id', :'model_name', :'model_name',
    'Codex 訂閱', ARRAY['openai_responses'], true, true, true, true, false, 'active'
);
COMMIT;
