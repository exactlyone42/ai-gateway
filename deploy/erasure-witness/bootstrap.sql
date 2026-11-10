INSERT INTO erasure_witness.position (singleton, install, sequence, tip)
VALUES (true, :'install'::uuid, :'sequence'::bigint, decode(:'tip', 'hex'));
