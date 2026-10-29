use sha2::{Digest, Sha256};
use sqlx::PgPool;

async fn reserve(pool: &PgPool, fingerprint: &[u8]) -> Option<i64> {
    sqlx::query_scalar(
        "INSERT INTO capture_master_key_usage (key_fingerprint, wrap_count) VALUES ($1, 1) \
         ON CONFLICT (key_fingerprint) DO UPDATE SET wrap_count = capture_master_key_usage.wrap_count + 1 \
         WHERE capture_master_key_usage.wrap_count < 1048576 RETURNING wrap_count",
    )
    .bind(fingerprint)
    .fetch_optional(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn master_key_usage_is_atomic_bounded_and_cannot_be_reset(pool: PgPool) {
    let key = [17u8; 32];
    let fingerprint = Sha256::digest(key).to_vec();
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let pool = pool.clone();
        let fingerprint = fingerprint.clone();
        tasks.push(tokio::spawn(async move {
            reserve(&pool, &fingerprint).await.unwrap()
        }));
    }
    let mut observed = Vec::new();
    for task in tasks {
        observed.push(task.await.unwrap());
    }
    observed.sort();
    assert_eq!(observed, (1..=16).collect::<Vec<_>>());
    assert!(
        sqlx::query(
            "UPDATE capture_master_key_usage SET wrap_count = 1 WHERE key_fingerprint = $1"
        )
        .bind(&fingerprint)
        .execute(&pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM capture_master_key_usage WHERE key_fingerprint = $1")
            .bind(&fingerprint)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(sqlx::query("UPDATE capture_master_key_usage SET key_fingerprint = $2, wrap_count = 17 WHERE key_fingerprint = $1")
        .bind(&fingerprint).bind(vec![2u8; 32]).execute(&pool).await.is_err());
    assert!(
        sqlx::query("TRUNCATE capture_master_key_usage")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO capture_master_key_usage (key_fingerprint, wrap_count) VALUES ($1, 1)"
        )
        .bind(vec![1u8; 31])
        .execute(&pool)
        .await
        .is_err()
    );
    let exhausted = Sha256::digest([19u8; 32]).to_vec();
    sqlx::query(
        "INSERT INTO capture_master_key_usage (key_fingerprint, wrap_count) VALUES ($1, 1048575)",
    )
    .bind(&exhausted)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(reserve(&pool, &exhausted).await, Some(1048576));
    assert_eq!(reserve(&pool, &exhausted).await, None);
}
