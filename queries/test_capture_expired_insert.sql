WITH owner AS (
    INSERT INTO users (id, tenant_id, display_name)
    VALUES ($1, $2, 'maintenance-fixture')
    RETURNING id, tenant_id
)
INSERT INTO capture_records (tenant_id, capture_id, user_id, key_fingerprint, wrapped_key, created_at, expires_at, state)
SELECT tenant_id, $3, id, decode(repeat('01', 32), 'hex'), decode(repeat('02', 60), 'hex'),
       now() - interval '2 days', now() - interval '1 day', 'complete'
FROM owner
