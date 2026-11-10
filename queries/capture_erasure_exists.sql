SELECT EXISTS (SELECT 1 FROM capture_erasure_marks WHERE tenant_id = $1 AND capture_id = $2) AS "exists!"
