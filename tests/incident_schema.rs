use sqlx::{PgPool, Row as _};
use uuid::Uuid;

const TENANT_A: Uuid = Uuid::from_u128(800);
const TENANT_B: Uuid = Uuid::from_u128(801);

#[sqlx::test]
async fn incident_history_is_tenant_safe_and_append_only(pool: PgPool) {
    let admin_a = insert_admin(&pool, TENANT_A, Uuid::from_u128(802), "admin-a").await;
    let admin_b = insert_admin(&pool, TENANT_B, Uuid::from_u128(803), "admin-b").await;
    let incident_id = Uuid::from_u128(804);
    sqlx::query(
        r#"
        INSERT INTO incidents (
            id, tenant_id, title, status, service, impact_scope, started_at,
            summary, impact, created_by, updated_by
        ) VALUES ($1, $2, 'Gateway timeout', 'investigating', 'gateway', 'system',
                  now(), 'Requests stopped before first byte.', 'Admin requests were delayed.', $3, $3)
        "#,
    )
    .bind(incident_id)
    .bind(TENANT_A)
    .bind(admin_a)
    .execute(&pool)
    .await
    .unwrap();

    let cross_tenant_actor = sqlx::query(
        r#"
        INSERT INTO incident_events (
            id, tenant_id, incident_id, actor_admin_id, event_type, message
        ) VALUES ($1, $2, $3, $4, 'timeline', 'wrong tenant')
        "#,
    )
    .bind(Uuid::from_u128(805))
    .bind(TENANT_A)
    .bind(incident_id)
    .bind(admin_b)
    .execute(&pool)
    .await;
    assert!(
        cross_tenant_actor.is_err(),
        "a cross-tenant actor wrote an event"
    );

    let event_id = Uuid::from_u128(806);
    sqlx::query(
        r#"
        INSERT INTO incident_events (
            id, tenant_id, incident_id, actor_admin_id, event_type, message,
            before_state, after_state
        ) VALUES ($1, $2, $3, $4, 'status_changed', 'Mitigation applied',
                  '{"status":"investigating"}', '{"status":"mitigated"}')
        "#,
    )
    .bind(event_id)
    .bind(TENANT_A)
    .bind(incident_id)
    .bind(admin_a)
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "UPDATE incidents SET status = 'mitigated', version = version + 1, updated_by = $1 WHERE tenant_id = $2 AND id = $3",
    )
    .bind(admin_a)
    .bind(TENANT_A)
    .bind(incident_id)
    .execute(&pool)
    .await
    .unwrap();
    let version: i64 =
        sqlx::query("SELECT version FROM incidents WHERE tenant_id = $1 AND id = $2")
            .bind(TENANT_A)
            .bind(incident_id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("version");
    assert_eq!(version, 2);

    let update_event =
        sqlx::query("UPDATE incident_events SET message = 'rewritten' WHERE id = $1")
            .bind(event_id)
            .execute(&pool)
            .await;
    assert!(update_event.is_err(), "an incident event was rewritten");
    let delete_event = sqlx::query("DELETE FROM incident_events WHERE id = $1")
        .bind(event_id)
        .execute(&pool)
        .await;
    assert!(delete_event.is_err(), "an incident event was deleted");
    let delete_incident = sqlx::query("DELETE FROM incidents WHERE id = $1")
        .bind(incident_id)
        .execute(&pool)
        .await;
    assert!(
        delete_incident.is_err(),
        "an incident and its history were deleted"
    );

    let invalid_status = sqlx::query(
        r#"
        INSERT INTO incidents (
            id, tenant_id, title, status, service, impact_scope, started_at,
            summary, impact, created_by, updated_by
        ) VALUES ($1, $2, 'Invalid', 'done', 'gateway', 'system', now(),
                  'summary', 'impact', $3, $3)
        "#,
    )
    .bind(Uuid::from_u128(807))
    .bind(TENANT_A)
    .bind(admin_a)
    .execute(&pool)
    .await;
    assert!(
        invalid_status.is_err(),
        "an unknown incident status was stored"
    );
}

async fn insert_admin(pool: &PgPool, tenant_id: Uuid, id: Uuid, name: &str) -> Uuid {
    sqlx::query(
        "INSERT INTO admin_accounts (id, tenant_id, display_name, role) VALUES ($1, $2, $3, 'admin')",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
    id
}
