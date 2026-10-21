WITH actors AS (
    SELECT DISTINCT ON (tenant_id) tenant_id, id AS actor_id
    FROM admin_accounts
    WHERE status = 'active'
    ORDER BY tenant_id, created_at, id
), imported AS (
    INSERT INTO incidents (
        id, tenant_id, title, status, service, impact_scope, started_at, ended_at,
        summary, impact, root_cause, temporary_fix, permanent_fix, prevention,
        validation_evidence, created_by, updated_by
    )
    SELECT
        md5(tenant_id::text || ':2026-10-21-gateway-timeout')::uuid,
        tenant_id,
        'Claude Code 工作跑滿 15 分鐘卻沒有產物',
        'mitigated',
        'gateway',
        'system',
        '2026-10-21 13:05:00+08'::timestamptz,
        '2026-10-21 13:20:00+08'::timestamptz,
        'Gateway 同時請求上限、部署中斷及首筆回應等待設定共同讓長工作無法完成。',
        '50 個請求中，10 個排隊逾時、11 個約 60 秒後由呼叫端中止；工作跑滿 15 分鐘仍沒有產物。',
        'Gateway 把同憑證、模型與供應來源的同時執行上限設為 1，並在仍有請求時部署；模型清單也未宣告較長的首筆回應等待。',
        '停止以 Gateway 自身同時請求上限攔截，並避免在長請求進行中部署。',
        '模型清單與本機 Client 的靜默等待提高為 600 秒；供應商 429 不再觸發唯一供應商暫停。',
        '忙碌時自動延後部署仍待完成；後續需持續監看排隊逾時與部署中斷。',
        '4 個真實並行請求均在 5 秒內完成；舊上限設為 1 時，100 個自動測試請求仍全部放行。',
        actor_id,
        actor_id
    FROM actors
    ON CONFLICT (id) DO NOTHING
    RETURNING id, tenant_id, created_by, created_at
)
INSERT INTO incident_events (
    id, tenant_id, incident_id, actor_admin_id, event_type, occurred_at, message,
    deployment_version
)
SELECT
    md5(tenant_id::text || ':2026-10-21-gateway-timeout:import')::uuid,
    tenant_id,
    id,
    created_by,
    'verification',
    created_at,
    '由既有事故文件匯入；忙碌時自動延後部署尚未完成。',
    '90cb105a84a9'
FROM imported;

WITH actors AS (
    SELECT DISTINCT ON (tenant_id) tenant_id, id AS actor_id, role
    FROM admin_accounts
    WHERE status = 'active'
    ORDER BY tenant_id, created_at, id
)
INSERT INTO audit_logs (
    id, tenant_id, actor_admin_id, actor_role, action, target_type, target_id,
    after_state, occurred_at, request_id
)
SELECT
    md5(actors.tenant_id::text || ':2026-10-21-gateway-timeout:audit')::uuid,
    actors.tenant_id,
    actors.actor_id,
    actors.role,
    'incident.import',
    'incident',
    incidents.id,
    jsonb_build_object('id', incidents.id, 'status', incidents.status, 'version', incidents.version),
    now(),
    'migration-20261021153500'
FROM actors
JOIN incidents ON incidents.tenant_id = actors.tenant_id
    AND incidents.id = md5(actors.tenant_id::text || ':2026-10-21-gateway-timeout')::uuid
ON CONFLICT (id) DO NOTHING;
