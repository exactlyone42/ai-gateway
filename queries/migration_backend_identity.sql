SELECT pid AS "pid!", backend_start AS "started!"
FROM pg_catalog.pg_stat_activity
WHERE pid = pg_catalog.pg_backend_pid();
