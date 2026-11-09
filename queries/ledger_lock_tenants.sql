WITH tenants AS MATERIALIZED (
    SELECT DISTINCT unnest($1::uuid[]) AS id ORDER BY id
)
SELECT pg_advisory_xact_lock(hashtextextended(id::text, 12012)) FROM tenants
