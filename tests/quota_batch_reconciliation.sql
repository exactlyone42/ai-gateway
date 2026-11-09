DO $test$
DECLARE
    tenant UUID;
    quota UUID;
    sentinel UUID;
    reservation UUID;
    request UUID;
    context INTEGER;
    item INTEGER;
    estimated BIGINT;
    estimated_cost BIGINT;
    moment TIMESTAMPTZ := date_trunc('hour', now());
    payload JSONB := '[]';
    allocation JSONB;
    snapshot JSONB;
    observed BIGINT;
BEGIN
    FOR context IN 1..2 LOOP
        tenant := gen_random_uuid();
        quota := gen_random_uuid();
        INSERT INTO quotas (id, tenant_id, quota_key, revision, configuration, lifecycle_state)
        VALUES (quota, tenant, 'batch', 1, '{"mode":"hard","limits":{"tokens_per_hour":10000}}', 'active');
        INSERT INTO quota_windows (tenant_id, quota_id, limit_kind, window_start, window_end, reserved_tokens, reserved_cost_microusd)
        VALUES
            (tenant, quota, 'tokens_per_hour', moment, moment + interval '1 hour', 265, 0),
            (tenant, quota, 'tokens_per_hour', moment - interval '1 hour', moment, 530, 0),
            (tenant, quota, 'tokens_per_day', moment, moment + interval '1 day', 265, 0),
            (tenant, quota, 'cost_per_day_microusd', moment, moment + interval '1 day', 0, 300);
        sentinel := gen_random_uuid();
        INSERT INTO quotas (id, tenant_id, quota_key, revision, configuration, lifecycle_state)
        VALUES (sentinel, tenant, 'untouched', 1, '{"mode":"hard","limits":{"tokens_per_hour":10000}}', 'active');
        INSERT INTO quota_windows (tenant_id, quota_id, limit_kind, window_start, window_end, reserved_tokens)
        VALUES (tenant, sentinel, 'tokens_per_hour', moment, moment + interval '1 hour', 37);
        FOR item IN 1..2 LOOP
            reservation := gen_random_uuid();
            request := gen_random_uuid();
            estimated := CASE WHEN item = 1 THEN 119 ELSE 146 END;
            estimated_cost := CASE WHEN item = 1 THEN 100 ELSE 200 END;
            SELECT jsonb_agg(jsonb_build_object(
                'quota_id', quota, 'quota_key', 'batch', 'quota_revision', 1,
                'limit_kind', w.limit_kind, 'window_start', w.window_start, 'window_end', w.window_end,
                'reserved_requests', 0,
                'reserved_tokens', CASE WHEN w.limit_kind = 'cost_per_day_microusd' THEN 0
                    WHEN w.window_start < moment THEN estimated * 2 ELSE estimated END,
                'reserved_cost_microusd', CASE WHEN w.limit_kind = 'cost_per_day_microusd' THEN estimated_cost ELSE 0 END
            ) ORDER BY w.limit_kind, w.window_start) INTO allocation
            FROM quota_windows w WHERE w.tenant_id = tenant AND w.quota_id = quota;
            INSERT INTO quota_reservations (id, tenant_id, gateway_request_id, state, estimated_tokens,
                estimated_cost_microusd, decision, allocations, expires_at)
            VALUES (reservation, tenant, request, 'active', estimated, estimated_cost,
                '{}', allocation, now() + interval '1 day');
            payload := payload || jsonb_build_array(jsonb_build_object(
                'request', jsonb_build_object(
                    'id', request, 'tenant_id', tenant, 'request_id', 'batch-' || context || '-' || item,
                    'status', 'completed', 'created_at', now(), 'started_at', now(), 'completed_at', now(),
                    'estimated_tokens', estimated, 'quota_reservation_id', reservation,
                    'quota_decision', '{}'::jsonb, 'quota_actual_tokens', 2,
                    'quota_actual_cost_microusd', CASE WHEN item = 1 THEN 3 ELSE 5 END
                ), 'attempts', '[]'::jsonb, 'usage_records', '[]'::jsonb
            ));
        END LOOP;
    END LOOP;
    PERFORM ingest_usage_envelopes(payload);
    SELECT reserved_tokens INTO observed FROM quota_windows
    WHERE tenant_id = tenant AND quota_id = quota AND limit_kind = 'tokens_per_hour' AND window_start = moment;
    IF observed <> 4 THEN
        RAISE EXCEPTION 'batch window expected 4, observed %', observed;
    END IF;
    IF EXISTS (SELECT 1 FROM quota_windows w JOIN quotas q ON (q.tenant_id, q.id) = (w.tenant_id, w.quota_id)
        WHERE q.quota_key = 'batch' AND (
        (limit_kind <> 'cost_per_day_microusd' AND reserved_tokens <> 4)
        OR (limit_kind = 'cost_per_day_microusd' AND reserved_cost_microusd <> 8))) THEN
        RAISE EXCEPTION 'batch reconciliation mixed tenant, window, limit or cost deltas';
    END IF;
    IF (SELECT count(*) FROM quota_reconciliation_events) <> 4
        OR (SELECT count(*) FROM quota_reservations WHERE state = 'reconciled' AND actual_tokens = 2) <> 4 THEN
        RAISE EXCEPTION 'batch reconciliation lost a reservation or event';
    END IF;
    SELECT jsonb_agg(to_jsonb(w) ORDER BY tenant_id, quota_id, limit_kind, window_start)
    INTO snapshot FROM quota_windows w;
    PERFORM ingest_usage_envelopes(payload);
    IF snapshot IS DISTINCT FROM (SELECT jsonb_agg(to_jsonb(w) ORDER BY tenant_id, quota_id, limit_kind, window_start) FROM quota_windows w)
        OR (SELECT count(*) FROM quota_reconciliation_events) <> 4 THEN
        RAISE EXCEPTION 'identical batch replay changed quota';
    END IF;
    payload := jsonb_set(payload, '{0,request,quota_actual_tokens}', '3');
    BEGIN
        PERFORM ingest_usage_envelopes(payload);
        RAISE EXCEPTION 'conflicting replay was accepted';
    EXCEPTION WHEN SQLSTATE 'GW409' THEN
        NULL;
    END;
    IF snapshot IS DISTINCT FROM (SELECT jsonb_agg(to_jsonb(w) ORDER BY tenant_id, quota_id, limit_kind, window_start) FROM quota_windows w)
        OR (SELECT count(*) FROM quota_reconciliation_events) <> 4 THEN
        RAISE EXCEPTION 'conflicting batch changed quota';
    END IF;
    reservation := gen_random_uuid();
    request := gen_random_uuid();
    SELECT jsonb_agg(a || jsonb_build_object(
        'reserved_tokens', CASE WHEN a->>'limit_kind' = 'cost_per_day_microusd' THEN 0 ELSE 10 END,
        'reserved_cost_microusd', CASE WHEN a->>'limit_kind' = 'cost_per_day_microusd' THEN 100 ELSE 0 END
    )) INTO allocation FROM jsonb_array_elements(allocation) a;
    INSERT INTO quota_reservations (id, tenant_id, gateway_request_id, state, estimated_tokens,
        estimated_cost_microusd, decision, allocations, expires_at)
    VALUES (reservation, tenant, request, 'active', 10, 100, '{}', allocation, now() + interval '1 day');
    UPDATE quota_windows SET
        reserved_tokens = reserved_tokens + CASE WHEN limit_kind = 'cost_per_day_microusd' THEN 0 ELSE 10 END,
        reserved_cost_microusd = reserved_cost_microusd + CASE WHEN limit_kind = 'cost_per_day_microusd' THEN 100 ELSE 0 END
    WHERE tenant_id = tenant AND quota_id = quota;
    payload := jsonb_set(payload, '{0,request,quota_actual_tokens}', '2');
    payload := jsonb_build_array(payload->0, jsonb_build_object(
        'request', jsonb_build_object(
            'id', request, 'tenant_id', tenant, 'request_id', 'mixed-replay-new',
            'status', 'completed', 'created_at', now(), 'started_at', now(), 'completed_at', now(),
            'estimated_tokens', 10, 'quota_reservation_id', reservation,
            'quota_decision', '{}'::jsonb, 'quota_actual_tokens', 20, 'quota_actual_cost_microusd', 30
        ), 'attempts', '[]'::jsonb, 'usage_records', '[]'::jsonb
    ));
    SELECT jsonb_agg(to_jsonb(w) ORDER BY tenant_id, quota_id, limit_kind, window_start)
    INTO snapshot FROM quota_windows w;
    BEGIN
        PERFORM ingest_usage_envelopes(jsonb_set(payload, '{0,request,quota_actual_tokens}', '3'));
        RAISE EXCEPTION 'mixed conflicting replay was accepted';
    EXCEPTION WHEN SQLSTATE 'GW409' THEN
        NULL;
    END;
    IF snapshot IS DISTINCT FROM (SELECT jsonb_agg(to_jsonb(w) ORDER BY tenant_id, quota_id, limit_kind, window_start) FROM quota_windows w)
        OR (SELECT count(*) FROM quota_reconciliation_events) <> 4
        OR EXISTS (SELECT 1 FROM gateway_requests WHERE id = request)
        OR NOT EXISTS (SELECT 1 FROM quota_reservations WHERE id = reservation AND state = 'active') THEN
        RAISE EXCEPTION 'mixed conflicting batch partially committed';
    END IF;
    PERFORM ingest_usage_envelopes(payload);
    IF EXISTS (SELECT 1 FROM quota_windows WHERE tenant_id = tenant AND quota_id = quota AND (
        (limit_kind <> 'cost_per_day_microusd' AND reserved_tokens <> 24)
        OR (limit_kind = 'cost_per_day_microusd' AND reserved_cost_microusd <> 38)))
        OR (SELECT count(*) FROM quota_reconciliation_events) <> 5 THEN
        RAISE EXCEPTION 'mixed replay and fresh request changed quota incorrectly';
    END IF;
    IF EXISTS (SELECT 1 FROM quota_windows w JOIN quotas q ON (q.tenant_id, q.id) = (w.tenant_id, w.quota_id)
        WHERE w.tenant_id <> tenant AND q.quota_key = 'batch' AND (
        (limit_kind <> 'cost_per_day_microusd' AND reserved_tokens <> 4)
        OR (limit_kind = 'cost_per_day_microusd' AND reserved_cost_microusd <> 8))) THEN
        RAISE EXCEPTION 'mixed replay changed another tenant window';
    END IF;
    IF EXISTS (SELECT 1 FROM quota_windows w JOIN quotas q ON (q.tenant_id, q.id) = (w.tenant_id, w.quota_id)
        WHERE q.quota_key = 'untouched' AND (reserved_tokens <> 37 OR reserved_cost_microusd <> 0)) THEN
        RAISE EXCEPTION 'batch changed an unrelated quota';
    END IF;
    SELECT jsonb_agg(to_jsonb(w) ORDER BY tenant_id, quota_id, limit_kind, window_start)
    INTO snapshot FROM quota_windows w;
    PERFORM ingest_usage_envelopes(payload);
    IF snapshot IS DISTINCT FROM (SELECT jsonb_agg(to_jsonb(w) ORDER BY tenant_id, quota_id, limit_kind, window_start) FROM quota_windows w)
        OR (SELECT count(*) FROM quota_reconciliation_events) <> 5 THEN
        RAISE EXCEPTION 'mixed replay settled a request twice';
    END IF;
END;
$test$;
