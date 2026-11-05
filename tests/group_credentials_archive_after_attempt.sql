CREATE FUNCTION archive_group_provider_after_attempt() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.request_id = 'subject-changed' AND NEW.action = 'admin.group_credential.replace_attempt' THEN
        UPDATE providers SET lifecycle_state = 'archived', version = version + 1
        WHERE tenant_id = NEW.tenant_id AND provider_key = 'anthropic-test';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER archive_group_provider_after_attempt AFTER INSERT ON audit_logs
FOR EACH ROW EXECUTE FUNCTION archive_group_provider_after_attempt();
