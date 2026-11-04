SELECT pg_catalog.pg_cancel_backend(pid) AS "cancelled!"
FROM pg_catalog.pg_stat_activity
WHERE pid = $1 AND backend_start = $2
  AND datname = pg_catalog.current_database()
  AND usename = current_user AND state = 'active';
