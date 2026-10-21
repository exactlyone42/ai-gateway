CREATE INDEX gateway_requests_admin_cursor_idx
    ON gateway_requests (tenant_id, created_at DESC, id DESC);

CREATE INDEX audit_logs_admin_cursor_idx
    ON audit_logs (tenant_id, occurred_at DESC, id DESC);
