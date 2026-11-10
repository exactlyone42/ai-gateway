CREATE TABLE capture_erasure_marks (
    tenant_id uuid NOT NULL,
    capture_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, capture_id)
);

INSERT INTO capture_erasure_marks (tenant_id, capture_id)
SELECT tenant_id, capture_id FROM capture_records WHERE state IN ('deleting', 'deleted')
ON CONFLICT DO NOTHING;
