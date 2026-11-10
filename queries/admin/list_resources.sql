SELECT id AS "id!", resource - $5::text[] AS "resource!"
    FROM (
        SELECT 'users' AS kind, id, tenant_id, to_jsonb(users) AS resource FROM users
        UNION ALL SELECT 'devices', id, tenant_id, to_jsonb(devices) FROM devices
        UNION ALL SELECT 'clients', id, tenant_id, to_jsonb(clients) FROM clients
        UNION ALL SELECT 'agents', id, tenant_id, to_jsonb(agents) FROM agents
        UNION ALL SELECT 'credentials', id, tenant_id, to_jsonb(credentials) FROM credentials
        UNION ALL SELECT 'providers', id, tenant_id, to_jsonb(providers) FROM providers
        UNION ALL SELECT 'models', id, tenant_id, to_jsonb(models) FROM models
        UNION ALL SELECT 'model-aliases', id, tenant_id, to_jsonb(model_aliases) FROM model_aliases
        UNION ALL SELECT 'model-task-profiles', id, tenant_id, to_jsonb(model_task_profiles) FROM model_task_profiles
        UNION ALL SELECT 'task-classifier-configs', id, tenant_id, to_jsonb(task_classifier_configs) FROM task_classifier_configs
        UNION ALL SELECT 'policies', id, tenant_id, to_jsonb(policies) FROM policies
        UNION ALL SELECT 'quotas', id, tenant_id, to_jsonb(quotas) FROM quotas
    ) resources
    WHERE kind = $1 AND tenant_id = $2 AND id > $3
      AND ($6::text IS NULL OR (kind = 'users' AND lower(resource->>'email') = lower($6)))
    ORDER BY id LIMIT $4
