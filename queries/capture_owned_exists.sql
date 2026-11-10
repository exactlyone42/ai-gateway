SELECT EXISTS (
    SELECT 1 FROM capture_records
    WHERE tenant_id = $1 AND user_id = $2 AND capture_id = $3
) AS "exists!"
