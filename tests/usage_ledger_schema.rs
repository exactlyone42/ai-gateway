use sqlx::{Executor as _, PgPool};
use uuid::Uuid;

const TENANT: Uuid = Uuid::from_u128(1);
const REQUEST_ONE: Uuid = Uuid::from_u128(2);
const REQUEST_TWO: Uuid = Uuid::from_u128(3);
const ATTEMPT_ONE: Uuid = Uuid::from_u128(4);

#[sqlx::test]
async fn ledger_schema_is_tenant_safe_idempotent_and_append_only(pool: PgPool) {
    insert_request(&pool, REQUEST_ONE, "request-one").await;
    insert_request(&pool, REQUEST_TWO, "request-two").await;
    insert_attempt(&pool).await;
    insert_usage(&pool, Uuid::from_u128(5), REQUEST_ONE, "provider_reported")
        .await
        .unwrap();

    let duplicate = insert_usage(&pool, Uuid::from_u128(6), REQUEST_ONE, "provider_reported").await;
    assert!(duplicate.is_err(), "a replay with a new id was accepted");

    let wrong_request = insert_usage(&pool, Uuid::from_u128(7), REQUEST_TWO, "estimated").await;
    assert!(
        wrong_request.is_err(),
        "usage was attached to an attempt from another request"
    );

    let update = sqlx::query("UPDATE usage_records SET input_tokens = 999")
        .execute(&pool)
        .await;
    assert!(update.is_err(), "append-only rows accepted an update");

    let truncate = sqlx::query("TRUNCATE usage_records").execute(&pool).await;
    assert!(truncate.is_err(), "append-only rows accepted a truncate");

    sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'retained')")
        .bind(Uuid::from_u128(8))
        .bind(TENANT)
        .execute(&pool)
        .await
        .unwrap();
    let delete_identity = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(Uuid::from_u128(8))
        .execute(&pool)
        .await;
    assert!(
        delete_identity.is_err(),
        "an identity could disappear before its delayed ledger flush"
    );
}

#[sqlx::test]
async fn all_nine_acceptance_queries_are_executable(pool: PgPool) {
    pool.execute(include_str!("../docs/usage-ledger-queries.sql"))
        .await
        .unwrap();
}

async fn insert_request(pool: &PgPool, id: Uuid, request_id: &str) {
    sqlx::query(
        r#"
        INSERT INTO gateway_requests (
            id, tenant_id, request_id, workload_class, urgency, quality,
            budget_class, requested_model, status, created_at
        ) VALUES ($1, $2, $3, 'interactive', 'immediate', 'standard',
                  'normal', 'company-default', 'completed', now())
        "#,
    )
    .bind(id)
    .bind(TENANT)
    .bind(request_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_attempt(pool: &PgPool) {
    sqlx::query(
        r#"
        INSERT INTO provider_attempts (
            id, tenant_id, gateway_request_id, provider, model,
            attempt_number, started_at, completed_at, status,
            estimated_usage, actual_usage
        ) VALUES ($1, $2, $3, 'test-provider', 'company-default', 1,
                  now(), now(), 'completed',
                  '{"input_tokens": 8, "cached_input_tokens": 2}',
                  '{"input_tokens": 10, "output_tokens": 3,
                    "cached_input_tokens": 4, "cache_write_input_tokens": 2,
                    "reasoning_tokens": 1}')
        "#,
    )
    .bind(ATTEMPT_ONE)
    .bind(TENANT)
    .bind(REQUEST_ONE)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_usage(
    pool: &PgPool,
    id: Uuid,
    request_id: Uuid,
    source: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO usage_records (
            id, tenant_id, gateway_request_id, provider_attempt_id, source,
            input_tokens, output_tokens, cached_input_tokens,
            cache_write_input_tokens, reasoning_tokens, recorded_at
        ) VALUES ($1, $2, $3, $4, $5, 10, 3, 4, 2, 1, now())
        "#,
    )
    .bind(id)
    .bind(TENANT)
    .bind(request_id)
    .bind(ATTEMPT_ONE)
    .bind(source)
    .execute(pool)
    .await
    .map(|_| ())
}
