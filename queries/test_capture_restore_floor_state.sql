SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'capture_restore_incomplete'),
       install::text, sequence, encode(tip, 'hex')
FROM capture_erasure_floor WHERE singleton = true;
