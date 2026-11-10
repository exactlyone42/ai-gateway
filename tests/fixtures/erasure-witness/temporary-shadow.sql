SET ROLE gateway_witness_test;
CREATE TEMP TABLE position (singleton boolean, install uuid, sequence bigint, tip bytea);
INSERT INTO position VALUES (true, :'install'::uuid, 0, decode(:'tip', 'hex'));
SELECT erasure_witness.advance(
    :'install'::uuid,
    0,
    decode(:'tip', 'hex'),
    decode(:'next_tip', 'hex')
);
SELECT count(*) FROM pg_temp.position WHERE sequence = 0;
