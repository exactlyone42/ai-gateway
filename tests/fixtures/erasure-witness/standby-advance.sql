SET ROLE gateway_witness_login;
SELECT erasure_witness.advance('00000000-0000-4000-8000-000000000001'::uuid, 0, decode(repeat('01', 32), 'hex'), decode(repeat('02', 32), 'hex'));
RESET ROLE;
