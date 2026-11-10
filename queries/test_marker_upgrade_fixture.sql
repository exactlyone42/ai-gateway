INSERT INTO users (id, tenant_id, display_name)
VALUES ('10000000-0000-0000-0000-000000000001', '20000000-0000-0000-0000-000000000002', 'migration-test');
INSERT INTO capture_records (tenant_id, capture_id, user_id, key_fingerprint, wrapped_key, created_at, expires_at, state)
VALUES ('20000000-0000-0000-0000-000000000002', '30000000-0000-0000-0000-000000000003', '10000000-0000-0000-0000-000000000001', decode(repeat('01', 32), 'hex'), NULL, now(), now() + interval '1 day', 'deleted');
