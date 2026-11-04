BEGIN;
SELECT capture_id FROM capture_records
WHERE tenant_id = :'tenant'::uuid AND capture_id = :'capture'::uuid
FOR UPDATE \g /dev/null
\echo READY
