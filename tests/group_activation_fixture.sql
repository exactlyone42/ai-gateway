INSERT INTO policies (id, tenant_id, policy_key, revision, group_id, configuration, lifecycle_state) VALUES
('00000000-0000-0000-0000-000000001012','00000000-0000-0000-0000-000000000100','replacement',1,'00000000-0000-0000-0000-000000000010','{}','draft');
UPDATE quotas SET quota_key = 'first', revision = 3
WHERE id = '00000000-0000-0000-0000-000000002011';
INSERT INTO quotas (id, tenant_id, quota_key, revision, group_id, configuration, lifecycle_state) VALUES
('00000000-0000-0000-0000-000000002012','00000000-0000-0000-0000-000000000100','first',2,'00000000-0000-0000-0000-000000000010','{}','draft');
