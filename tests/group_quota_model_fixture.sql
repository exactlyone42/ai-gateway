INSERT INTO providers (id, tenant_id, provider_key, display_name, provider_type, base_url, credential_ref)
VALUES ('00000000-0000-0000-0000-000000003001', '00000000-0000-0000-0000-000000000100', 'fixture', 'Fixture', 'openai', 'https://fixture.invalid', 'env://FIXTURE');
INSERT INTO models (id, tenant_id, provider_id, public_model_id, provider_model_id, display_name)
VALUES ('00000000-0000-0000-0000-000000003002', '00000000-0000-0000-0000-000000000100', '00000000-0000-0000-0000-000000003001', 'fixture', 'fixture', 'Fixture');
INSERT INTO groups (id, tenant_id, display_name)
VALUES ('00000000-0000-0000-0000-000000000012', '00000000-0000-0000-0000-000000000100', 'not a member');
