SET ROLE gateway_witness_test;
SELECT erasure_witness.advance(
    :'install'::uuid,
    :'sequence'::bigint,
    decode(:'tip', 'hex'),
    decode(:'next_tip', 'hex')
);
