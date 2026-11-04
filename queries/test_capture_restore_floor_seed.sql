INSERT INTO capture_erasure_floor (singleton, install, sequence, tip)
VALUES (true, :'install'::uuid, :'sequence'::bigint, decode(:'tip', 'hex'));
