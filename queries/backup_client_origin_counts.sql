SELECT count(*) FILTER (WHERE client_addr = :'gateway_ip'::inet),
       count(*) FILTER (WHERE client_addr IS DISTINCT FROM :'gateway_ip'::inet)
FROM pg_catalog.pg_stat_activity
WHERE datname = current_database()
  AND backend_type = 'client backend'
  AND pid <> pg_backend_pid();
