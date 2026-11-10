SELECT EXISTS (
    SELECT 1 FROM pg_stat_activity
    WHERE datname = current_database() AND pid <> pg_backend_pid()
      AND wait_event_type = 'Lock'
      AND query LIKE 'SELECT id FROM groups WHERE tenant_id%FOR SHARE%'
) AS "waiting!";
