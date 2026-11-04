SET ROLE gateway_witness_test;
INSERT INTO erasure_witness.position (install, sequence, tip)
VALUES (:'install'::uuid, 0, decode(:'tip', 'hex'));
