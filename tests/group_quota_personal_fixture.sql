INSERT INTO quotas (id, tenant_id, quota_key, revision, user_id, configuration, lifecycle_state)
VALUES ('00000000-0000-0000-0000-000000002001', '00000000-0000-0000-0000-000000000100', 'first', 2,
        '00000000-0000-0000-0000-000000000001', '{"mode":"hard","limits":{"requests_per_minute":100}}', 'active')
