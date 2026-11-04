SELECT
    (SELECT count(*) FROM pg_catalog.pg_stat_activity
     WHERE datid = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())
       AND pid <> pg_backend_pid())
    + (SELECT count(*) FROM pg_catalog.pg_prepared_xacts WHERE database = current_database());
