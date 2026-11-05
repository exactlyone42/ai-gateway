BEGIN;
INSERT INTO users (id, tenant_id, display_name) VALUES
    ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000100', 'user');
INSERT INTO groups (id, tenant_id, display_name) VALUES
    ('00000000-0000-0000-0000-000000000010', '00000000-0000-0000-0000-000000000100', 'first'),
    ('00000000-0000-0000-0000-000000000020', '00000000-0000-0000-0000-000000000200', 'other');
INSERT INTO policies (id, tenant_id, policy_key, revision, group_id, configuration, lifecycle_state)
VALUES ('00000000-0000-0000-0000-000000001001', '00000000-0000-0000-0000-000000000100', 'group', 1,
        '00000000-0000-0000-0000-000000000010', '{}', 'active');
INSERT INTO quotas (id, tenant_id, quota_key, revision, group_id, configuration, lifecycle_state)
VALUES ('00000000-0000-0000-0000-000000002001', '00000000-0000-0000-0000-000000000100', 'group', 1,
        '00000000-0000-0000-0000-000000000010', '{}', 'active');
DO $$
BEGIN
    BEGIN
        INSERT INTO policies (id, tenant_id, policy_key, revision, group_id, configuration)
        VALUES (gen_random_uuid(), '00000000-0000-0000-0000-000000000100', 'cross', 1,
                '00000000-0000-0000-0000-000000000020', '{}');
        RAISE EXCEPTION 'cross tenant policy accepted';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO quotas (id, tenant_id, quota_key, revision, group_id, configuration)
        VALUES (gen_random_uuid(), '00000000-0000-0000-0000-000000000100', 'cross', 1,
                '00000000-0000-0000-0000-000000000020', '{}');
        RAISE EXCEPTION 'cross tenant quota accepted';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO policies (id, tenant_id, policy_key, revision, group_id, user_id, configuration)
        VALUES (gen_random_uuid(), '00000000-0000-0000-0000-000000000100', 'mixed', 1,
                '00000000-0000-0000-0000-000000000010', '00000000-0000-0000-0000-000000000001', '{}');
        RAISE EXCEPTION 'mixed policy accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO quotas (id, tenant_id, quota_key, revision, group_id, user_id, configuration)
        VALUES (gen_random_uuid(), '00000000-0000-0000-0000-000000000100', 'mixed', 1,
                '00000000-0000-0000-0000-000000000010', '00000000-0000-0000-0000-000000000001', '{}');
        RAISE EXCEPTION 'mixed quota accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO policies (id, tenant_id, policy_key, revision, group_id, configuration, lifecycle_state)
        VALUES (gen_random_uuid(), '00000000-0000-0000-0000-000000000100', 'duplicate', 1,
                '00000000-0000-0000-0000-000000000010', '{}', 'active');
        RAISE EXCEPTION 'duplicate policy accepted';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO quotas (id, tenant_id, quota_key, revision, group_id, configuration, lifecycle_state)
        VALUES (gen_random_uuid(), '00000000-0000-0000-0000-000000000100', 'group', 2,
                '00000000-0000-0000-0000-000000000010', '{}', 'active');
        RAISE EXCEPTION 'duplicate quota accepted';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;
END $$;
ROLLBACK;
