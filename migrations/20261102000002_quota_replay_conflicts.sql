ALTER FUNCTION ingest_usage_envelopes(JSONB)
    RENAME TO ingest_usage_envelopes_before_quota_conflict_check;

CREATE FUNCTION ingest_usage_envelopes(payload JSONB) RETURNS VOID
LANGUAGE plpgsql AS $$
BEGIN
    IF jsonb_typeof(payload) <> 'array' THEN
        RAISE EXCEPTION 'usage envelope payload must be an array';
    END IF;

    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION 'usage ingestion requires read committed isolation' USING ERRCODE = 'GW410';
    END IF;

    PERFORM pg_advisory_xact_lock(hashtextextended(tenant_id::text, 12012))
    FROM (
        SELECT DISTINCT (envelope->'request'->>'tenant_id')::uuid AS tenant_id
        FROM jsonb_array_elements(payload) AS envelope
        WHERE envelope->'request'->>'tenant_id' IS NOT NULL
        ORDER BY tenant_id
    ) tenants;

    IF EXISTS (
        SELECT 1
        FROM jsonb_array_elements(payload) AS envelope
        CROSS JOIN LATERAL jsonb_to_record(envelope->'request') AS request(
            tenant_id UUID, quota_reservation_id UUID, status TEXT,
            quota_actual_tokens BIGINT, quota_actual_cost_microusd BIGINT
        )
        JOIN quota_reconciliation_events AS existing
          ON existing.tenant_id = request.tenant_id
         AND existing.reservation_id = request.quota_reservation_id
         AND existing.event_kind = 'final'
        WHERE request.quota_actual_tokens IS NOT NULL
          AND request.quota_actual_cost_microusd IS NOT NULL
          AND (existing.request_status, existing.actual_tokens, existing.actual_cost_microusd)
              IS DISTINCT FROM (request.status, request.quota_actual_tokens, request.quota_actual_cost_microusd)
    ) OR EXISTS (
        SELECT 1
        FROM jsonb_array_elements(payload) AS envelope
        JOIN quota_reservations AS reservation
          ON reservation.tenant_id = (envelope->'request'->>'tenant_id')::uuid
         AND reservation.gateway_request_id = (envelope->'request'->>'id')::uuid
        WHERE reservation.id IS DISTINCT FROM (envelope->'request'->>'quota_reservation_id')::uuid
    ) OR EXISTS (
        SELECT 1
        FROM jsonb_array_elements(payload) AS envelope
        JOIN quota_reservations AS reservation
          ON reservation.tenant_id = (envelope->'request'->>'tenant_id')::uuid
         AND reservation.id = (envelope->'request'->>'quota_reservation_id')::uuid
        WHERE reservation.gateway_request_id IS DISTINCT FROM (envelope->'request'->>'id')::uuid
    ) OR EXISTS (
        SELECT 1
        FROM jsonb_array_elements(payload) AS envelope
        JOIN gateway_requests AS existing
          ON existing.tenant_id = (envelope->'request'->>'tenant_id')::uuid
         AND existing.id = (envelope->'request'->>'id')::uuid
        WHERE existing.quota_reservation_id IS DISTINCT FROM (envelope->'request'->>'quota_reservation_id')::uuid
    ) OR EXISTS (
        SELECT 1
        FROM jsonb_array_elements(payload) AS envelope
        GROUP BY (envelope->'request'->>'tenant_id')::uuid, (envelope->'request'->>'id')::uuid
        HAVING count(DISTINCT envelope) > 1
    ) OR EXISTS (
        SELECT 1
        FROM jsonb_array_elements(payload) AS envelope
        CROSS JOIN LATERAL jsonb_to_record(envelope->'request') AS request(
            tenant_id UUID, quota_reservation_id UUID, status TEXT,
            quota_actual_tokens BIGINT, quota_actual_cost_microusd BIGINT
        )
        WHERE request.quota_reservation_id IS NOT NULL
          AND request.quota_actual_tokens IS NOT NULL
          AND request.quota_actual_cost_microusd IS NOT NULL
        GROUP BY request.tenant_id, request.quota_reservation_id
        HAVING count(DISTINCT (request.status, request.quota_actual_tokens, request.quota_actual_cost_microusd)) > 1
    ) OR EXISTS (
        SELECT 1
        FROM jsonb_array_elements(payload) AS envelope
        JOIN gateway_requests AS existing
          ON existing.tenant_id = (envelope->'request'->>'tenant_id')::uuid
         AND existing.id = (envelope->'request'->>'id')::uuid
        CROSS JOIN LATERAL jsonb_array_elements(envelope->'attempts') AS attempt
        WHERE NOT EXISTS (
            SELECT 1 FROM provider_attempts
            WHERE tenant_id = existing.tenant_id
              AND gateway_request_id = existing.id
              AND id = (attempt->>'id')::uuid
        )
    ) OR EXISTS (
        SELECT 1
        FROM jsonb_array_elements(payload) AS envelope
        JOIN gateway_requests AS existing
          ON existing.tenant_id = (envelope->'request'->>'tenant_id')::uuid
         AND existing.id = (envelope->'request'->>'id')::uuid
        CROSS JOIN LATERAL jsonb_array_elements(envelope->'usage_records') AS usage
        WHERE NOT EXISTS (
            SELECT 1 FROM usage_records
            WHERE tenant_id = existing.tenant_id
              AND gateway_request_id = existing.id
              AND id = (usage->>'id')::uuid
        )
    ) THEN
        RAISE EXCEPTION 'conflicting quota reconciliation in usage replay' USING ERRCODE = 'GW409';
    END IF;

    PERFORM ingest_usage_envelopes_before_quota_conflict_check(payload);
END;
$$;
