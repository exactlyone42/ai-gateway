INSERT INTO quotas (
                        id, tenant_id, quota_key, revision, user_id, device_id, client_id,
                        agent_id, credential_id, model_id, provider_id, configuration, group_id
                    ) VALUES (
                        $1, $2, $3, $4, $5, $6, $7, $8,
                        COALESCE((
                            SELECT COALESCE(rotation_root_id, id)
                            FROM credentials WHERE tenant_id = $2 AND id = $9
                        ), $9),
                        $10, $11, $12, $13
                    )
                    RETURNING to_jsonb(quotas) AS "value!"
