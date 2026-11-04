SELECT pg_try_advisory_xact_lock($1::integer, $2::integer) AS "locked!"
