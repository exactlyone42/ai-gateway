BEGIN;

SET LOCAL ROLE :"witness_owner";

DO $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_roles AS owner_role
        WHERE owner_role.rolname = current_user
          AND NOT owner_role.rolcanlogin
          AND NOT EXISTS (
              SELECT 1 FROM pg_catalog.pg_auth_members
              WHERE member = owner_role.oid OR roleid = owner_role.oid
          )
          AND NOT EXISTS (
              SELECT 1 FROM pg_catalog.pg_roles AS inherited
              WHERE pg_catalog.pg_has_role(owner_role.oid, inherited.oid, 'MEMBER')
                AND (
                    inherited.rolsuper
                    OR inherited.rolcreaterole
                    OR inherited.rolcreatedb
                    OR inherited.rolreplication
                    OR inherited.rolbypassrls
                    OR inherited.oid = (
                        SELECT datdba FROM pg_catalog.pg_database
                        WHERE datname = pg_catalog.current_database()
                    )
                )
          )
    ) THEN
        RAISE EXCEPTION '見證擁有者必須是無登入權限的非特權角色';
    END IF;
END
$guard$;

CREATE SCHEMA erasure_witness;
REVOKE ALL ON SCHEMA erasure_witness FROM PUBLIC;

CREATE TABLE erasure_witness.position (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    install uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence >= 0),
    tip bytea NOT NULL CHECK (octet_length(tip) = 32)
);

REVOKE ALL ON erasure_witness.position FROM PUBLIC;

CREATE FUNCTION erasure_witness.advance(
    expected_install uuid,
    expected_sequence bigint,
    expected_tip bytea,
    next_tip bytea
) RETURNS boolean
LANGUAGE sql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $function$
    WITH updated AS (
        UPDATE erasure_witness.position
        SET sequence = sequence + 1, tip = next_tip
        WHERE singleton = true
          AND install = expected_install
          AND sequence = expected_sequence
          AND tip = expected_tip
          AND sequence < 9223372036854775807
          AND octet_length(next_tip) = 32
        RETURNING 1
    )
    SELECT EXISTS (SELECT 1 FROM updated)
$function$;

REVOKE ALL ON FUNCTION erasure_witness.advance(uuid, bigint, bytea, bytea) FROM PUBLIC;

DO $guard$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_catalog.pg_namespace AS namespace
        CROSS JOIN LATERAL pg_catalog.aclexplode(
            COALESCE(namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))
        ) AS privilege
        WHERE namespace.nspname = 'erasure_witness'
          AND privilege.grantee <> namespace.nspowner
    ) OR EXISTS (
        SELECT 1 FROM pg_catalog.pg_class AS relation
        JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace
        CROSS JOIN LATERAL pg_catalog.aclexplode(
            COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner))
        ) AS privilege
        WHERE namespace.nspname = 'erasure_witness' AND relation.relname = 'position'
          AND privilege.grantee <> relation.relowner
    ) OR EXISTS (
        SELECT 1 FROM pg_catalog.pg_proc AS procedure
        JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = procedure.pronamespace
        CROSS JOIN LATERAL pg_catalog.aclexplode(
            COALESCE(procedure.proacl, pg_catalog.acldefault('f', procedure.proowner))
        ) AS privilege
        WHERE namespace.nspname = 'erasure_witness' AND procedure.proname = 'advance'
          AND privilege.grantee <> procedure.proowner
    ) THEN
        RAISE EXCEPTION '見證結構包含多餘授權';
    END IF;
END
$guard$;

COMMIT;
