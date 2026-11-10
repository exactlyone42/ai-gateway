SELECT pg_catalog.pg_advisory_lock(4706339964786332754) \g /dev/null
WITH owner AS (
    INSERT INTO users (id, tenant_id, display_name)
    VALUES (:'user'::uuid, :'tenant'::uuid, 'expired-fixture')
    RETURNING id, tenant_id
)
INSERT INTO capture_records (tenant_id, capture_id, user_id, key_fingerprint, wrapped_key, created_at, expires_at, state)
SELECT tenant_id, :'capture'::uuid, id, decode(repeat('01', 32), 'hex'), decode(repeat('02', 60), 'hex'),
       now() - interval '2 days', now() - interval '1 day', 'complete'
FROM owner;
\echo READY
