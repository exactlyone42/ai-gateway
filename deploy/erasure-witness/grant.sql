BEGIN;

GRANT USAGE ON SCHEMA erasure_witness TO :"witness_role";
GRANT SELECT ON erasure_witness.position TO :"witness_role";
GRANT EXECUTE ON FUNCTION erasure_witness.advance(uuid, bigint, bytea, bytea) TO :"witness_role";

SELECT pg_catalog.set_config('erasure_witness.grant_role', :'witness_role', true);

DO $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_roles AS caller
        WHERE caller.rolname = pg_catalog.current_setting('erasure_witness.grant_role')
          AND NOT EXISTS (
              SELECT 1 FROM pg_catalog.pg_roles AS inherited
              WHERE pg_catalog.pg_has_role(caller.oid, inherited.oid, 'MEMBER')
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
                    OR EXISTS (
                        SELECT 1 FROM pg_catalog.pg_proc AS routine
                        JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = routine.pronamespace
                        WHERE routine.prosecdef
                          AND routine.oid <> 'erasure_witness.advance(uuid,bigint,bytea,bytea)'::regprocedure
                          AND pg_catalog.has_schema_privilege(inherited.oid, namespace.oid, 'USAGE')
                          AND pg_catalog.has_function_privilege(inherited.oid, routine.oid, 'EXECUTE')
                    )
                )
          )
    ) THEN
        RAISE EXCEPTION '見證使用者角色具有多餘權限';
    END IF;
END
$guard$;

COMMIT;
