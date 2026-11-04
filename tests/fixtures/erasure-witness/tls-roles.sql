CREATE ROLE gateway_witness_tls_owner NOLOGIN;
CREATE ROLE gateway_witness_login LOGIN PASSWORD 'synthetic-only';
GRANT CREATE ON DATABASE :"test_database" TO gateway_witness_tls_owner;
