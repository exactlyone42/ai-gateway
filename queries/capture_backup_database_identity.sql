SELECT system_identifier::text, oid::text
FROM pg_control_system(), pg_database
WHERE datname = current_database();
