INSERT INTO providers (id, tenant_id, provider_key, display_name, provider_type, base_url, credential_ref, supported_operations, lifecycle_state)
VALUES ('40000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000001', 'ollama-stop-test', 'test', 'local', $1, 'env://TEST', ARRAY['openai_responses'], 'active')
