SELECT EXISTS (
    SELECT 1 FROM pg_catalog.pg_stat_activity
    WHERE datname = pg_catalog.current_database()
      AND pid <> pg_catalog.pg_backend_pid()
      AND wait_event_type = 'Lock'
      AND query LIKE '%INSERT INTO _sqlx_migrations%'
) AS "waiting!";
