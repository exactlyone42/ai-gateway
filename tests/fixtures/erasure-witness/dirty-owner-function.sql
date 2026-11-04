GRANT CREATE ON DATABASE witness_dirty TO gateway_witness_dirty_owner;
ALTER DEFAULT PRIVILEGES FOR ROLE gateway_witness_dirty_owner
GRANT EXECUTE ON FUNCTIONS TO gateway_witness_ungranted;
