use sqlx::{PgPool, Row as _};
use uuid::Uuid;

const TENANT_A: Uuid = Uuid::from_u128(100);
const TENANT_B: Uuid = Uuid::from_u128(200);

#[sqlx::test]
async fn admin_schema_enforces_tenant_bootstrap_and_draft_boundaries(pool: PgPool) {
    let admin_id = Uuid::from_u128(101);
    let tenant_b_admin_id = Uuid::from_u128(102);
    let tenant_b_credential_id = Uuid::from_u128(103);
    sqlx::query(
        "INSERT INTO admin_accounts (id, tenant_id, display_name, role) VALUES ($1, $2, 'owner', 'admin')",
    )
    .bind(admin_id)
    .bind(TENANT_A)
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("INSERT INTO admin_bootstrap_claims (tenant_id, admin_account_id) VALUES ($1, $2)")
        .bind(TENANT_A)
        .bind(admin_id)
        .execute(&pool)
        .await
        .unwrap();
    let second_claim = sqlx::query(
        "INSERT INTO admin_bootstrap_claims (tenant_id, admin_account_id) VALUES ($1, $2)",
    )
    .bind(TENANT_A)
    .bind(admin_id)
    .execute(&pool)
    .await;
    assert!(second_claim.is_err(), "bootstrap could be consumed twice");

    let update_claim =
        sqlx::query("UPDATE admin_bootstrap_claims SET consumed_at = now() WHERE tenant_id = $1")
            .bind(TENANT_A)
            .execute(&pool)
            .await;
    assert!(update_claim.is_err(), "bootstrap claim could be updated");
    let delete_claim = sqlx::query("DELETE FROM admin_bootstrap_claims WHERE tenant_id = $1")
        .bind(TENANT_A)
        .execute(&pool)
        .await;
    assert!(delete_claim.is_err(), "bootstrap claim could be deleted");
    let truncate = sqlx::query("TRUNCATE admin_bootstrap_claims")
        .execute(&pool)
        .await;
    assert!(truncate.is_err(), "bootstrap claims could be truncated");

    sqlx::query(
        "INSERT INTO admin_accounts (id, tenant_id, display_name, role) VALUES ($1, $2, 'tenant-b-owner', 'admin')",
    )
    .bind(tenant_b_admin_id)
    .bind(TENANT_B)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO admin_login_credentials (
            id, tenant_id, admin_account_id, display_name, secret_hash,
            hash_key_id, key_prefix, fingerprint
        ) VALUES ($1, $2, $3, 'tenant-b-login', $4, 'v1', 'prefix', $5)
        "#,
    )
    .bind(tenant_b_credential_id)
    .bind(TENANT_B)
    .bind(tenant_b_admin_id)
    .bind([3_u8; 32].as_slice())
    .bind([4_u8; 32].as_slice())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO admin_sessions (
            id, tenant_id, admin_account_id, login_credential_id,
            token_hash, hash_key_id, fingerprint, expires_at, source_ip,
            account_version, login_credential_version
        ) VALUES ($1, $2, $3, $4, $5, 'v1', $6,
                  now() + interval '5 minutes', '127.0.0.1', 1, 1)
        "#,
    )
    .bind(Uuid::from_u128(104))
    .bind(TENANT_B)
    .bind(tenant_b_admin_id)
    .bind(tenant_b_credential_id)
    .bind([5_u8; 32].as_slice())
    .bind([6_u8; 32].as_slice())
    .execute(&pool)
    .await
    .expect("same-tenant session fixture must be valid");

    let cross_tenant_session = sqlx::query(
        r#"
        INSERT INTO admin_sessions (
            id, tenant_id, admin_account_id, login_credential_id,
            token_hash, hash_key_id, fingerprint, expires_at, source_ip,
            account_version, login_credential_version
        ) VALUES ($1, $2, $3, $4, $5, 'v1', $6,
                  now() + interval '5 minutes', '127.0.0.1', 1, 1)
        "#,
    )
    .bind(Uuid::from_u128(105))
    .bind(TENANT_B)
    .bind(admin_id)
    .bind(tenant_b_credential_id)
    .bind([1_u8; 32].as_slice())
    .bind([2_u8; 32].as_slice())
    .execute(&pool)
    .await;
    let error = cross_tenant_session.expect_err("a cross-tenant session was accepted");
    let database = error
        .as_database_error()
        .expect("expected a database error");
    assert_eq!(database.code().as_deref(), Some("23503"));
    assert_eq!(database.constraint(), Some("admin_sessions_account_fk"));

    let active_provider = sqlx::query(
        r#"
        INSERT INTO providers (
            id, tenant_id, provider_key, display_name, provider_type,
            base_url, credential_ref, lifecycle_state
        ) VALUES ($1, $2, 'provider', 'provider', 'openai',
                  'https://example.invalid', 'env://PROVIDER_KEY', 'active')
        "#,
    )
    .bind(Uuid::from_u128(106))
    .bind(TENANT_A)
    .execute(&pool)
    .await;
    assert!(
        active_provider.is_err(),
        "W08 accepted a provider state the data plane does not enforce"
    );
}

#[sqlx::test]
async fn migration_keeps_legacy_identity_and_audit_writes_compatible(pool: PgPool) {
    let user_id = Uuid::from_u128(110);
    sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'legacy')")
        .bind(user_id)
        .bind(TENANT_A)
        .execute(&pool)
        .await
        .unwrap();
    let row = sqlx::query("SELECT status, version FROM users WHERE tenant_id = $1 AND id = $2")
        .bind(TENANT_A)
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("status"), "active");
    assert_eq!(row.get::<i64, _>("version"), 1);

    let audit_id = Uuid::from_u128(111);
    sqlx::query(
        r#"
        INSERT INTO audit_logs (
            id, tenant_id, action, target_type, target_id, occurred_at
        ) VALUES ($1, $2, 'legacy.action', 'user', $3, now())
        "#,
    )
    .bind(audit_id)
    .bind(TENANT_A)
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();
    let row = sqlx::query(
        "SELECT actor_admin_id, actor_role, source_ip::text AS source_ip, request_id FROM audit_logs WHERE id = $1",
    )
    .bind(audit_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(row.get::<Option<Uuid>, _>("actor_admin_id").is_none());
    assert!(row.get::<Option<String>, _>("actor_role").is_none());
    assert!(row.get::<Option<String>, _>("source_ip").is_none());
    assert!(row.get::<Option<String>, _>("request_id").is_none());

    let mutation = sqlx::query("UPDATE audit_logs SET action = 'rewritten' WHERE id = $1")
        .bind(audit_id)
        .execute(&pool)
        .await;
    assert!(
        mutation.is_err(),
        "legacy audit row was no longer append-only"
    );
}
