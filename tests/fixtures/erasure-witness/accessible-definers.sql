SET ROLE gateway_witness_test;
SELECT count(*) FROM pg_catalog.pg_proc AS routine
JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = routine.pronamespace
WHERE routine.prosecdef
  AND routine.oid <> 'erasure_witness.advance(uuid,bigint,bytea,bytea)'::regprocedure
  AND pg_catalog.has_schema_privilege(current_user, namespace.oid, 'USAGE')
  AND pg_catalog.has_function_privilege(current_user, routine.oid, 'EXECUTE');
RESET ROLE;
