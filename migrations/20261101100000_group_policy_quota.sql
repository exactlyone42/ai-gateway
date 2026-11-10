ALTER TABLE policies
    ADD COLUMN group_id UUID,
    ADD CONSTRAINT policies_group_tenant_fk FOREIGN KEY (tenant_id, group_id)
        REFERENCES groups (tenant_id, id),
    ADD CONSTRAINT policies_group_scope_check CHECK (
        group_id IS NULL OR num_nonnulls(user_id, device_id, client_id, agent_id, credential_id) = 0
    );

ALTER TABLE quotas
    ADD COLUMN group_id UUID,
    ADD CONSTRAINT quotas_group_tenant_fk FOREIGN KEY (tenant_id, group_id)
        REFERENCES groups (tenant_id, id),
    ADD CONSTRAINT quotas_group_scope_check CHECK (
        group_id IS NULL OR num_nonnulls(user_id, device_id, client_id, agent_id, credential_id, model_id, provider_id) = 0
    );

DROP INDEX policies_active_scope_idx;
CREATE UNIQUE INDEX policies_active_scope_idx ON policies (
    tenant_id,
    (CASE
        WHEN group_id IS NOT NULL THEN 'group'
        WHEN user_id IS NOT NULL THEN 'user'
        WHEN device_id IS NOT NULL THEN 'device'
        WHEN client_id IS NOT NULL THEN 'client'
        WHEN agent_id IS NOT NULL THEN 'agent'
        WHEN credential_id IS NOT NULL THEN 'credential'
        ELSE 'global'
    END),
    (COALESCE(group_id, user_id, device_id, client_id, agent_id, credential_id,
        '00000000-0000-0000-0000-000000000000'::uuid))
) WHERE lifecycle_state = 'active';

DROP INDEX quotas_active_scope_idx;
CREATE UNIQUE INDEX quotas_active_scope_idx ON quotas (
    tenant_id, quota_key,
    (CASE
        WHEN group_id IS NOT NULL THEN 'group'
        WHEN user_id IS NOT NULL THEN 'user'
        WHEN device_id IS NOT NULL THEN 'device'
        WHEN client_id IS NOT NULL THEN 'client'
        WHEN agent_id IS NOT NULL THEN 'agent'
        WHEN credential_id IS NOT NULL THEN 'credential'
        WHEN model_id IS NOT NULL THEN 'model'
        WHEN provider_id IS NOT NULL THEN 'provider'
        ELSE 'global'
    END),
    (COALESCE(group_id, user_id, device_id, client_id, agent_id, credential_id, model_id, provider_id,
        '00000000-0000-0000-0000-000000000000'::uuid))
) WHERE lifecycle_state = 'active';
