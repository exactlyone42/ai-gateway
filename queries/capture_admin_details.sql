SELECT user_id, (extract(epoch FROM created_at) * 1000)::bigint AS "created_ms!", (extract(epoch FROM (expires_at - created_at)) / 86400)::integer AS "days!", key_fingerprint
FROM capture_records
WHERE tenant_id = $1 AND capture_id = $2 AND state IN ('complete', 'incomplete') AND expires_at > now()
