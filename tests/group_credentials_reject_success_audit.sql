CREATE FUNCTION reject_group_credential_audit() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.request_id = 'group-audit-failure' AND NEW.action = 'admin.group_credential.replace_success' THEN
        RAISE EXCEPTION 'injected group credential audit failure';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER reject_group_credential_audit BEFORE INSERT ON audit_logs
FOR EACH ROW EXECUTE FUNCTION reject_group_credential_audit();
