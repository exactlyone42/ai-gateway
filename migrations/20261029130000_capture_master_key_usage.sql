CREATE TABLE capture_master_key_usage (
    key_fingerprint bytea PRIMARY KEY CHECK (octet_length(key_fingerprint) = 32),
    wrap_count bigint NOT NULL CHECK (wrap_count BETWEEN 1 AND 1048576)
);

CREATE FUNCTION protect_capture_master_key_usage() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.key_fingerprint = OLD.key_fingerprint AND NEW.wrap_count > OLD.wrap_count THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'capture master key usage cannot be reset';
END;
$$;

CREATE TRIGGER capture_master_key_usage_update
BEFORE UPDATE OR DELETE ON capture_master_key_usage
FOR EACH ROW EXECUTE FUNCTION protect_capture_master_key_usage();

CREATE TRIGGER capture_master_key_usage_truncate
BEFORE TRUNCATE ON capture_master_key_usage
FOR EACH STATEMENT EXECUTE FUNCTION protect_capture_master_key_usage();
