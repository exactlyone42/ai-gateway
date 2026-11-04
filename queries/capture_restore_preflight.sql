SELECT (SELECT count(*) FROM pg_namespace WHERE nspname NOT IN ('public', 'pg_catalog', 'information_schema') AND left(nspname, 3) <> 'pg_')
     + (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public')
     + (SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'public')
     + (SELECT count(*) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = 'public')
     + (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND backend_type = 'client backend' AND pid <> pg_backend_pid())
     + (SELECT count(*) FROM pg_prepared_xacts WHERE database = current_database());
