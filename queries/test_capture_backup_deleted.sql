SELECT state = 'deleted' AND wrapped_key IS NULL
FROM capture_records
WHERE tenant_id = :'tenant'::uuid AND capture_id = :'capture'::uuid;
