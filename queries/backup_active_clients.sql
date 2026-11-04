SELECT COALESCE(state, 'unknown'), count(*)
FROM pg_catalog.pg_stat_activity
WHERE datname = current_database()
  AND backend_type = 'client backend'
  AND pid <> pg_backend_pid()
GROUP BY state
ORDER BY state;
