WITH owner AS (
    INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'replay-test') RETURNING id, tenant_id
), record AS (
    INSERT INTO capture_records (tenant_id, capture_id, user_id, key_fingerprint, wrapped_key, created_at, expires_at, state)
    SELECT tenant_id, $3, id, decode(repeat('01', 32), 'hex'), decode(repeat('02', 60), 'hex'), now(), now() + interval '1 day', 'complete'
    FROM owner RETURNING tenant_id, capture_id
)
INSERT INTO capture_blocks (tenant_id, capture_id, leg, sequence, position, elapsed_ns, sealed)
SELECT tenant_id, capture_id, 0, n, n, n, decode(repeat('03', 28), 'hex')
FROM record CROSS JOIN generate_series(0, 69) AS n
