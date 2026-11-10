INSERT INTO policies (
                        id, tenant_id, policy_key, revision, user_id, device_id, client_id,
                        agent_id, credential_id, configuration, group_id
                    ) VALUES (
                        $1, $2, $3, $4, $5, $6, $7, $8,
                        COALESCE((
                            SELECT COALESCE(rotation_root_id, id)
                            FROM credentials WHERE tenant_id = $2 AND id = $9
                        ), $9),
                        $10, $11
                    )
                    RETURNING to_jsonb(policies) AS "value!"
