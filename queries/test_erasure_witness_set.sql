UPDATE erasure_witness.position
SET sequence = $2::bigint, tip = $3::bytea
WHERE singleton = true AND install = $1::uuid;
