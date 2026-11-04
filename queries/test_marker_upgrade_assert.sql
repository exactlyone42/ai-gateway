DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM capture_erasure_marks WHERE tenant_id = '20000000-0000-0000-0000-000000000002' AND capture_id = '30000000-0000-0000-0000-000000000003') THEN
        RAISE EXCEPTION 'missing existing erasure mark';
    END IF;
END $$;
