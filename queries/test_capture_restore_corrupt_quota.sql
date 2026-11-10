UPDATE quota_windows
SET reserved_requests = reserved_requests + 1
WHERE tenant_id = :'tenant'::uuid
  AND quota_id = :'quota'::uuid
  AND limit_kind = 'requests_per_minute'
  AND window_start = (
      SELECT min(window_start) FROM quota_windows
      WHERE tenant_id = :'tenant'::uuid AND quota_id = :'quota'::uuid
        AND limit_kind = 'requests_per_minute'
  )
RETURNING reserved_requests;
