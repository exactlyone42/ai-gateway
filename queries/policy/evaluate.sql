            WITH requested_credential AS (
                SELECT id, COALESCE(rotation_root_id, id) AS rotation_root_id
                FROM credentials WHERE tenant_id = $1 AND id = $6
            )
            SELECT policies.id, policies.policy_key, policies.revision, policies.configuration,
                   CASE
                     WHEN policies.credential_id IS NOT NULL THEN 'credential'
                     WHEN policies.agent_id IS NOT NULL THEN 'agent'
                     WHEN policies.client_id IS NOT NULL THEN 'client'
                     WHEN policies.device_id IS NOT NULL THEN 'device'
                     WHEN policies.user_id IS NOT NULL THEN 'user'
                     WHEN policies.group_id IS NOT NULL THEN 'group'
                     ELSE 'global'
                   END AS "scope!"
            FROM policies
            LEFT JOIN credentials target_credential
              ON target_credential.tenant_id = policies.tenant_id
             AND target_credential.id = policies.credential_id
            WHERE policies.tenant_id = $1 AND lifecycle_state = 'active'
              AND (
                (policies.group_id IS NULL AND policies.user_id IS NULL AND policies.device_id IS NULL
                 AND policies.client_id IS NULL AND policies.agent_id IS NULL
                 AND policies.credential_id IS NULL)
                OR policies.user_id = $2 OR policies.device_id = $3
                OR policies.client_id = $4 OR policies.agent_id = $5
                OR (policies.group_id IS NOT NULL AND EXISTS (
                    SELECT 1 FROM group_members gm
                    WHERE gm.tenant_id = policies.tenant_id
                      AND gm.group_id = policies.group_id AND gm.user_id = $2
                ))
                OR COALESCE(target_credential.rotation_root_id, target_credential.id) =
                   (SELECT rotation_root_id FROM requested_credential)
              )
            ORDER BY CASE
                WHEN policies.group_id IS NOT NULL THEN 1
                WHEN policies.user_id IS NOT NULL THEN 2
                WHEN policies.device_id IS NOT NULL THEN 3
                WHEN policies.client_id IS NOT NULL THEN 4
                WHEN policies.agent_id IS NOT NULL THEN 5
                WHEN policies.credential_id IS NOT NULL THEN 6
                ELSE 0
              END, policies.group_id, policies.id
