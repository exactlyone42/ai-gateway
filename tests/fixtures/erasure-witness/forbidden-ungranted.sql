SET ROLE gateway_witness_ungranted;
SELECT erasure_witness.advance(
    :'install'::uuid,
    :'sequence'::bigint,
    decode(:'tip', 'hex'),
    decode(:'next_tip', 'hex')
);
