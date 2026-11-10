SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'capture_restore_incomplete') AS "present!"
