SELECT count(*) AS "count!" FROM audit_logs
WHERE tenant_id = $1 AND (COALESCE(before_state::text, '') ILIKE '%example.com%'
    OR COALESCE(after_state::text, '') ILIKE '%example.com%')
