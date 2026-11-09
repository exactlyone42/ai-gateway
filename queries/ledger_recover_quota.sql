WITH candidates AS (
    SELECT (entry->'request'->>'id')::uuid AS gateway_request_id,
           (entry->'request'->>'tenant_id')::uuid AS tenant_id
    FROM jsonb_array_elements($1::jsonb) AS entry
    WHERE entry->'request'->>'quota_reservation_id' IS NULL
      AND entry->'request'->>'status' IN ('cancelled', 'failed')
      AND jsonb_array_length(entry->'attempts') = 0
)
SELECT reservation.tenant_id, reservation.gateway_request_id,
       reservation.id AS reservation_id, reservation.decision,
       EXISTS (
           SELECT 1 FROM jsonb_array_elements(reservation.allocations) AS allocation
           WHERE allocation->>'limit_kind' IN ('cost_per_day_microusd', 'cost_per_month_microusd')
       ) AS "requires_cost!"
FROM quota_reservations AS reservation
JOIN candidates USING (tenant_id, gateway_request_id)
WHERE reservation.state IN ('active', 'expired')
  AND NOT (reservation.id = ANY($2::uuid[]))
  AND NOT EXISTS (
      SELECT 1 FROM gateway_requests AS request
      WHERE request.tenant_id = reservation.tenant_id
        AND request.id = reservation.gateway_request_id
  )
