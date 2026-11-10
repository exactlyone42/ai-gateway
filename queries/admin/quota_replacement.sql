SELECT COALESCE(jsonb_agg(jsonb_build_object(
            'target_type', 'quota', 'id', current.id, 'before', to_jsonb(current)
        )), '[]'::jsonb) AS "value!"
        FROM quotas target
        JOIN quotas current
          ON current.tenant_id = target.tenant_id
         AND current.id <> target.id
         AND current.quota_key = target.quota_key
         AND current.group_id IS NOT DISTINCT FROM target.group_id
         AND current.user_id IS NOT DISTINCT FROM target.user_id
         AND current.device_id IS NOT DISTINCT FROM target.device_id
         AND current.client_id IS NOT DISTINCT FROM target.client_id
         AND current.agent_id IS NOT DISTINCT FROM target.agent_id
         AND current.credential_id IS NOT DISTINCT FROM target.credential_id
         AND current.model_id IS NOT DISTINCT FROM target.model_id
         AND current.provider_id IS NOT DISTINCT FROM target.provider_id
        WHERE target.tenant_id = $1 AND target.id = $2
          AND current.lifecycle_state = 'active'
