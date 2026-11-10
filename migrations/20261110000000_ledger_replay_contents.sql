ALTER FUNCTION ingest_usage_envelopes(JSONB)
    RENAME TO ingest_usage_envelopes_before_content_check;

CREATE FUNCTION ingest_usage_envelopes(payload JSONB) RETURNS VOID
LANGUAGE plpgsql AS $$
DECLARE
    expected JSONB;
    validation_pass INTEGER;
BEGIN
    IF jsonb_typeof(payload) IS DISTINCT FROM 'array' THEN
        RAISE EXCEPTION '用量資料必須是陣列';
    END IF;

    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION '用量入庫需要 read committed 交易隔離' USING ERRCODE = 'GW410';
    END IF;

    PERFORM pg_advisory_xact_lock(hashtextextended(tenant_id::text, 12012))
    FROM (
        SELECT DISTINCT (envelope->'request'->>'tenant_id')::uuid AS tenant_id
        FROM jsonb_array_elements(payload) AS envelope
        WHERE envelope->'request'->>'tenant_id' IS NOT NULL
        ORDER BY tenant_id
    ) tenants;

    SELECT COALESCE(jsonb_agg(jsonb_build_object(
        'request', to_jsonb(request),
        'attempts', COALESCE((
            SELECT jsonb_agg(DISTINCT to_jsonb(attempt) ORDER BY to_jsonb(attempt))
            FROM jsonb_array_elements(envelope->'attempts') AS value
            CROSS JOIN LATERAL jsonb_populate_record(NULL::provider_attempts,
                value || jsonb_build_object(
                    'provider_request_at', COALESCE(value->>'provider_request_at', value->>'started_at'),
                    'provider_first_byte_at', COALESCE(value->>'provider_first_byte_at', value->>'first_token_at'),
                    'provider_ttft', (value->>'provider_ttft_ms')::bigint * INTERVAL '1 millisecond'
                )
            ) AS attempt
        ), '[]'::jsonb),
        'usage_records', COALESCE((
            SELECT jsonb_agg(DISTINCT to_jsonb(usage) ORDER BY to_jsonb(usage))
            FROM jsonb_array_elements(envelope->'usage_records') AS value
            CROSS JOIN LATERAL jsonb_populate_record(NULL::usage_records, value) AS usage
        ), '[]'::jsonb),
        'parameters', COALESCE((
            SELECT jsonb_agg(to_jsonb(snapshot) ORDER BY to_jsonb(snapshot))
            FROM (VALUES
                ('requested', envelope->'request'->'requested_parameters'),
                ('provider', envelope->'request'->'provider_parameters')
            ) AS input(stage, parameters)
            CROSS JOIN LATERAL jsonb_populate_record(NULL::request_parameter_snapshots,
                jsonb_build_object(
                    'tenant_id', request.tenant_id, 'gateway_request_id', request.id,
                    'stage', input.stage, 'parameters', input.parameters,
                    'recorded_at', request.created_at
                )
            ) AS snapshot
            WHERE jsonb_typeof(input.parameters) = 'object'
        ), '[]'::jsonb),
        'quota_settlement', CASE
            WHEN request.quota_reservation_id IS NOT NULL
             AND envelope->'request'->>'quota_actual_tokens' IS NOT NULL
             AND envelope->'request'->>'quota_actual_cost_microusd' IS NOT NULL
            THEN jsonb_build_object(
                'tenant_id', request.tenant_id, 'reservation_id', request.quota_reservation_id,
                'event_kind', 'final', 'request_status', request.status,
                'actual_tokens', (envelope->'request'->>'quota_actual_tokens')::bigint,
                'actual_cost_microusd', (envelope->'request'->>'quota_actual_cost_microusd')::bigint,
                'occurred_at', request.completed_at
            )
        END,
        'attribution', (
            SELECT to_jsonb(attribution)
            FROM jsonb_populate_record(NULL::request_attribution,
                envelope->'request'->'attribution' || jsonb_build_object(
                    'tenant_id', request.tenant_id, 'gateway_request_id', request.id
                )
            ) AS attribution
            WHERE jsonb_typeof(envelope->'request'->'attribution') = 'object'
              AND envelope->'request'->'attribution' <> '{"prompt_id":null,"prompt_version":null,"skill_set_id":null,"agent_name":null,"agent_version":null,"client_version":null,"task_id":null}'::jsonb
        )
    )), '[]'::jsonb) INTO expected
    FROM jsonb_array_elements(payload) AS envelope
    CROSS JOIN LATERAL jsonb_populate_record(NULL::gateway_requests,
        ((envelope->'request') - ARRAY[
            'task_taxonomy_version', 'classified_task', 'classification_confidence',
            'capability_version', 'required_capabilities', 'service_objective_version',
            'service_objective', 'model_task_profile_id', 'model_task_profile_version'
        ]) || jsonb_build_object(
            'operation', COALESCE(envelope->'request'->>'operation', 'chat_completions'),
            'request_received_at', COALESCE(envelope->'request'->>'request_received_at', envelope->'request'->>'created_at'),
            'queue_duration', (envelope->'request'->>'queue_duration_ms')::bigint * INTERVAL '1 millisecond',
            'total_latency', (envelope->'request'->>'total_latency_ms')::bigint * INTERVAL '1 millisecond',
            'gateway_overhead', (envelope->'request'->>'gateway_overhead_ms')::bigint * INTERVAL '1 millisecond',
            'max_queue_time', (envelope->'request'->>'max_queue_time_ms')::bigint * INTERVAL '1 millisecond'
        )
    ) AS request;

    IF EXISTS (
        SELECT 1 FROM jsonb_array_elements(expected) AS envelope
        WHERE envelope->'request'->>'id' IS NULL
           OR envelope->'request'->>'tenant_id' IS NULL
           OR EXISTS (
               SELECT 1
               FROM (
                   SELECT value FROM jsonb_array_elements(envelope->'attempts')
                   UNION ALL
                   SELECT value FROM jsonb_array_elements(envelope->'usage_records')
               ) AS child
               WHERE child.value->'tenant_id' IS DISTINCT FROM envelope->'request'->'tenant_id'
                  OR child.value->'gateway_request_id' IS DISTINCT FROM envelope->'request'->'id'
           )
           OR EXISTS (
               SELECT 1 FROM jsonb_array_elements(envelope->'usage_records') AS usage
               WHERE usage->>'provider_attempt_id' IS NULL
                  OR NOT EXISTS (
                      SELECT 1 FROM jsonb_array_elements(envelope->'attempts') AS attempt
                      WHERE attempt->'id' = usage->'provider_attempt_id'
                  )
           )
    ) THEN
        RAISE EXCEPTION '用量或來源紀錄不屬於同一筆請求' USING ERRCODE = 'GW409';
    END IF;

    FOR validation_pass IN 1..2 LOOP
        IF EXISTS (
            SELECT 1
            FROM jsonb_array_elements(expected) AS envelope
            LEFT JOIN gateway_requests AS request ON request.id = (envelope->'request'->>'id')::uuid
            WHERE (validation_pass = 2 OR request.id IS NOT NULL)
              AND envelope IS DISTINCT FROM jsonb_build_object(
                  'request', to_jsonb(request),
                  'attempts', COALESCE((
                      SELECT jsonb_agg(to_jsonb(attempt) ORDER BY to_jsonb(attempt))
                      FROM provider_attempts AS attempt WHERE attempt.gateway_request_id = request.id
                  ), '[]'::jsonb),
                  'usage_records', COALESCE((
                      SELECT jsonb_agg(to_jsonb(usage) ORDER BY to_jsonb(usage))
                      FROM usage_records AS usage WHERE usage.gateway_request_id = request.id
                  ), '[]'::jsonb),
                  'parameters', COALESCE((
                      SELECT jsonb_agg(to_jsonb(snapshot) ORDER BY to_jsonb(snapshot))
                      FROM request_parameter_snapshots AS snapshot WHERE snapshot.gateway_request_id = request.id
                  ), '[]'::jsonb),
                  'quota_settlement', (
                      SELECT to_jsonb(settlement)
                      FROM quota_reconciliation_events AS settlement
                      WHERE settlement.tenant_id = request.tenant_id
                        AND settlement.reservation_id = request.quota_reservation_id
                        AND settlement.event_kind = 'final'
                  ),
                  'attribution', (
                      SELECT to_jsonb(attribution)
                      FROM request_attribution AS attribution WHERE attribution.gateway_request_id = request.id
                  )
              )
        ) THEN
            RAISE EXCEPTION '重播內容與已保存紀錄不同，整批未寫入' USING ERRCODE = 'GW409';
        END IF;

        IF validation_pass = 1 THEN
            BEGIN
                PERFORM ingest_usage_envelopes_before_content_check(payload);
            EXCEPTION WHEN foreign_key_violation THEN
                RAISE EXCEPTION '請求或用量的關聯衝突，整批未寫入' USING ERRCODE = 'GW409';
            END;
        END IF;
    END LOOP;
END;
$$;
