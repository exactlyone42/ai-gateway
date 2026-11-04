SELECT pg_backend_pid() AS "pid!", pg_advisory_xact_lock(hashtextextended($1::uuid::text, 12012))
