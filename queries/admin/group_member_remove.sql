DELETE FROM group_members WHERE tenant_id = $1 AND group_id = $2 AND user_id = $3 RETURNING user_id
