SELECT EXISTS (
    SELECT 1 FROM pg_catalog.pg_stat_activity
    WHERE datname = pg_catalog.current_database()
      AND pid <> pg_catalog.pg_backend_pid()
      AND wait_event = 'PgSleep'
      AND query LIKE '%capture_erasure_floor%'
) AS "waiting!";
