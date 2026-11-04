CREATE ROLE capture_identity_reader LOGIN;
REVOKE EXECUTE ON FUNCTION pg_control_system() FROM PUBLIC;
