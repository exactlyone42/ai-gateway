SELECT (
    NOT pg_catalog.pg_is_in_recovery()
    AND NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_roles AS inherited
        WHERE pg_catalog.pg_has_role(current_user, inherited.oid, 'MEMBER')
          AND (
              inherited.rolsuper
              OR pg_catalog.left(inherited.rolname, 3) = 'pg_'
              OR inherited.rolcreaterole
              OR inherited.rolcreatedb
              OR inherited.rolreplication
              OR inherited.rolbypassrls
              OR inherited.oid = (
                  SELECT datdba FROM pg_catalog.pg_database
                  WHERE datname = pg_catalog.current_database()
              )
              OR pg_catalog.has_schema_privilege(inherited.oid, 'erasure_witness', 'CREATE')
              OR pg_catalog.has_table_privilege(inherited.oid, 'erasure_witness.position', 'INSERT')
              OR pg_catalog.has_table_privilege(inherited.oid, 'erasure_witness.position', 'UPDATE')
              OR pg_catalog.has_table_privilege(inherited.oid, 'erasure_witness.position', 'DELETE')
              OR pg_catalog.has_table_privilege(inherited.oid, 'erasure_witness.position', 'TRUNCATE')
              OR pg_catalog.has_table_privilege(inherited.oid, 'erasure_witness.position', 'REFERENCES')
              OR pg_catalog.has_table_privilege(inherited.oid, 'erasure_witness.position', 'TRIGGER')
              OR pg_catalog.has_table_privilege(inherited.oid, 'erasure_witness.position', 'MAINTAIN')
              OR pg_catalog.has_any_column_privilege(inherited.oid, 'erasure_witness.position', 'INSERT')
              OR pg_catalog.has_any_column_privilege(inherited.oid, 'erasure_witness.position', 'UPDATE')
              OR pg_catalog.has_any_column_privilege(inherited.oid, 'erasure_witness.position', 'REFERENCES')
          )
    )
    AND NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_proc AS routine
        JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = routine.pronamespace
        JOIN pg_catalog.pg_roles AS inherited
          ON pg_catalog.pg_has_role(current_user, inherited.oid, 'MEMBER')
        WHERE routine.prosecdef
          AND routine.oid <> 'erasure_witness.advance(uuid,bigint,bytea,bytea)'::regprocedure
          AND pg_catalog.has_schema_privilege(inherited.oid, namespace.oid, 'USAGE')
          AND pg_catalog.has_function_privilege(inherited.oid, routine.oid, 'EXECUTE')
    )
) AS "safe!";
