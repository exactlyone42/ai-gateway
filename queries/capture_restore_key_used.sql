SELECT EXISTS (SELECT 1 FROM capture_master_key_usage WHERE key_fingerprint = decode(:'fingerprint', 'hex'));
