UPDATE policies current SET lifecycle_state = 'disabled',
                    version = current.version + 1, updated_at = now()
                FROM policies target
                WHERE target.tenant_id = $1 AND target.id = $2
                  AND current.tenant_id = target.tenant_id
                  AND current.id <> target.id AND current.lifecycle_state = 'active'
                  AND current.group_id IS NOT DISTINCT FROM target.group_id
                  AND current.user_id IS NOT DISTINCT FROM target.user_id
                  AND current.device_id IS NOT DISTINCT FROM target.device_id
                  AND current.client_id IS NOT DISTINCT FROM target.client_id
                  AND current.agent_id IS NOT DISTINCT FROM target.agent_id
                  AND current.credential_id IS NOT DISTINCT FROM target.credential_id
