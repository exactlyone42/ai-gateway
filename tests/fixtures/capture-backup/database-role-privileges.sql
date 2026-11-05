SELECT rolsuper, rolbypassrls, rolcreaterole, rolcreatedb, rolreplication,
       has_function_privilege(current_user, 'pg_catalog.pg_control_system()', 'EXECUTE')
FROM pg_roles
WHERE rolname = current_user;
