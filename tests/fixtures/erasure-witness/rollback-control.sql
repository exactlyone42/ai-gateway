UPDATE erasure_witness.position
SET sequence = :'sequence'::bigint, tip = decode(:'tip', 'hex')
WHERE singleton = true AND install = :'install'::uuid;
