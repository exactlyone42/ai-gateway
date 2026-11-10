CREATE FUNCTION test_ledger_conflict() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.request_id = 'backup-e2e-conflict' THEN
        RAISE EXCEPTION 'synthetic ledger conflict' USING ERRCODE = 'GW409';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER test_ledger_conflict BEFORE INSERT ON gateway_requests FOR EACH ROW EXECUTE FUNCTION test_ledger_conflict();
