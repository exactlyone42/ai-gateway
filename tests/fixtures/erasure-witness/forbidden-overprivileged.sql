SET ROLE :"test_role";
SELECT erasure_witness.advance(
    :'install'::uuid,
    :'sequence'::bigint,
    decode(:'tip', 'hex'),
    decode(:'next_tip', 'hex')
);
