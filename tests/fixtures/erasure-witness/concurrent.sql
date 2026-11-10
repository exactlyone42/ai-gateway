BEGIN;
SET LOCAL ROLE gateway_witness_test;
SELECT erasure_witness.advance(
    :'install'::uuid,
    :'sequence'::bigint,
    decode(:'tip', 'hex'),
    decode(:'next_tip', 'hex')
);
SELECT pg_sleep(2);
COMMIT;
