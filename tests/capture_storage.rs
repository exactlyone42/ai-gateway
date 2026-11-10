use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn capture_records_require_owner_tenant_and_bounded_expiry(pool: PgPool) {
    let tenant = Uuid::new_v4();
    let other = Uuid::new_v4();
    let user = Uuid::new_v4();
    let capture = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'test')")
        .bind(user)
        .bind(tenant)
        .execute(&pool)
        .await
        .unwrap();
    let insert = "INSERT INTO capture_records (tenant_id, capture_id, user_id, wrapped_key, expires_at, state, key_fingerprint) VALUES ($1, $2, $3, $4, now() + interval '30 days', 'recording', decode(repeat('01',32),'hex'))";
    assert!(
        sqlx::query(insert)
            .bind(other)
            .bind(capture)
            .bind(user)
            .bind(vec![1u8; 60])
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query(insert)
        .bind(tenant)
        .bind(capture)
        .bind(user)
        .bind(vec![1u8; 60])
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query(insert)
            .bind(tenant)
            .bind(capture)
            .bind(user)
            .bind(vec![1u8; 60])
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(sqlx::query("INSERT INTO capture_records (tenant_id, capture_id, user_id, wrapped_key, expires_at, state, key_fingerprint) VALUES ($1, $2, $3, $4, now() + interval '366 days', 'recording', decode(repeat('01',32),'hex'))")
        .bind(tenant).bind(Uuid::new_v4()).bind(user).bind(vec![1u8; 60]).execute(&pool).await.is_err());
    let block = "INSERT INTO capture_blocks (tenant_id, capture_id, leg, sequence, position, elapsed_ns, sealed) VALUES ($1, $2, 0, 0, 0, 0, $3)";
    assert!(
        sqlx::query(block)
            .bind(other)
            .bind(capture)
            .bind(vec![1u8; 28])
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(block)
            .bind(tenant)
            .bind(capture)
            .bind(vec![1u8; 27])
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query(block)
        .bind(tenant)
        .bind(capture)
        .bind(vec![1u8; 28])
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query(block)
            .bind(tenant)
            .bind(capture)
            .bind(vec![1u8; 28])
            .execute(&pool)
            .await
            .is_err()
    );
}
