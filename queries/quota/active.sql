    WITH requested_credential AS (
        SELECT COALESCE(rotation_root_id, id) AS root_id
        FROM credentials
        WHERE tenant_id = $1 AND id = $6
    )
    SELECT q.id, q.quota_key, q.revision, q.model_id, q.provider_id, q.configuration
    FROM quotas q
    LEFT JOIN credentials target_credential
      ON (target_credential.tenant_id, target_credential.id) = (q.tenant_id, q.credential_id)
    WHERE q.tenant_id = $1 AND q.lifecycle_state = 'active' AND (
        (q.group_id IS NULL AND q.user_id IS NULL AND q.device_id IS NULL AND q.client_id IS NULL
         AND q.agent_id IS NULL AND q.credential_id IS NULL
         AND q.model_id IS NULL AND q.provider_id IS NULL)
        OR q.user_id = $2 OR q.device_id = $3 OR q.client_id = $4 OR q.agent_id = $5
        OR (q.group_id IS NOT NULL AND EXISTS (
            SELECT 1 FROM group_members gm
            WHERE gm.tenant_id = q.tenant_id AND gm.group_id = q.group_id AND gm.user_id = $2
        ) AND NOT EXISTS (
            SELECT 1 FROM quotas user_quota
            WHERE user_quota.tenant_id = q.tenant_id AND user_quota.user_id = $2
              AND user_quota.quota_key = q.quota_key AND user_quota.lifecycle_state = 'active'
              AND user_quota.model_id IS NOT DISTINCT FROM q.model_id
              AND user_quota.provider_id IS NOT DISTINCT FROM q.provider_id
              AND user_quota.configuration->>'mode' = q.configuration->>'mode'
              AND NOT EXISTS (
                  SELECT 1 FROM jsonb_each(q.configuration->'limits') AS required(key, value)
                  WHERE required.value <> 'null'::jsonb
                    AND COALESCE(user_quota.configuration->'limits'->required.key, 'null'::jsonb) = 'null'::jsonb
              )
        ))
        OR COALESCE(target_credential.rotation_root_id, target_credential.id)
             = (SELECT root_id FROM requested_credential)
        OR (q.group_id IS NULL AND q.user_id IS NULL AND q.device_id IS NULL
            AND q.client_id IS NULL AND q.agent_id IS NULL AND q.credential_id IS NULL
            AND (q.model_id = ANY($7) OR q.provider_id = ANY($8)))
    )
    ORDER BY q.id
