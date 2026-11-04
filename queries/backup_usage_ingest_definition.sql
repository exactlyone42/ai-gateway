SELECT pg_get_functiondef(p.oid)
FROM pg_catalog.pg_proc AS p
JOIN pg_catalog.pg_namespace AS n ON n.oid = p.pronamespace
WHERE n.nspname = 'public'
  AND p.proname LIKE 'ingest_usage_envelopes%'
ORDER BY p.proname, p.oid;
