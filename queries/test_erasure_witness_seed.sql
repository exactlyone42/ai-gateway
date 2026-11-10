INSERT INTO erasure_witness.position (singleton, install, sequence, tip)
VALUES (true, $1::uuid, $2::bigint, $3::bytea);
