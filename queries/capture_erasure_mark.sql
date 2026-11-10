INSERT INTO capture_erasure_marks (tenant_id, capture_id) VALUES ($1, $2) ON CONFLICT DO NOTHING
