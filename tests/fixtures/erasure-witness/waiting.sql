SELECT count(*) FROM pg_stat_activity
WHERE datname = current_database() AND wait_event_type = 'Lock';
