SET ROLE gateway_witness_test;
SELECT install, sequence, encode(tip, 'hex') FROM erasure_witness.position;
