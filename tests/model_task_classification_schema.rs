use sqlx::{PgPool, Row as _};
use uuid::Uuid;

const TENANT_A: Uuid = Uuid::from_u128(700);
const TENANT_B: Uuid = Uuid::from_u128(701);

async fn insert_model(pool: &PgPool, tenant_id: Uuid, seed: u128) -> Uuid {
    let provider_id = Uuid::from_u128(seed);
    let model_id = Uuid::from_u128(seed + 1);
    sqlx::query(
        r#"
        INSERT INTO providers (
            id, tenant_id, provider_key, display_name, provider_type,
            base_url, credential_ref, supported_operations
        ) VALUES ($1, $2, $3, 'provider', 'openai',
                  'https://example.invalid', 'env://PROVIDER_KEY',
                  ARRAY['openai_responses'])
        "#,
    )
    .bind(provider_id)
    .bind(tenant_id)
    .bind(format!("provider-{seed}"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO models (
            id, tenant_id, provider_id, public_model_id, provider_model_id,
            display_name, supported_operations
        ) VALUES ($1, $2, $3, $4, $5, 'model', ARRAY['openai_responses'])
        "#,
    )
    .bind(model_id)
    .bind(tenant_id)
    .bind(provider_id)
    .bind(format!("public-{seed}"))
    .bind(format!("provider-model-{seed}"))
    .execute(pool)
    .await
    .unwrap();
    model_id
}

#[sqlx::test]
async fn task_profiles_are_versioned_and_tenant_safe(pool: PgPool) {
    let model_a = insert_model(&pool, TENANT_A, 710).await;
    let model_b = insert_model(&pool, TENANT_B, 720).await;
    let profile_id = Uuid::from_u128(730);

    sqlx::query(
        r#"
        INSERT INTO model_task_profiles (
            id, tenant_id, model_id, taxonomy_version, task_kind,
            suitability_score, confidence, source, source_ref, lifecycle_state
        ) VALUES ($1, $2, $3, 'task-taxonomy-v1', 'debugging',
                  90, 0.9500, 'manual', 'operator-review', 'active')
        "#,
    )
    .bind(profile_id)
    .bind(TENANT_A)
    .bind(model_a)
    .execute(&pool)
    .await
    .unwrap();

    let row = sqlx::query(
        "SELECT task_kind, confidence, source, version FROM model_task_profiles WHERE id = $1",
    )
    .bind(profile_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("task_kind"), "debugging");
    assert_eq!(row.get::<String, _>("source"), "manual");
    assert_eq!(row.get::<i64, _>("version"), 1);

    let cross_tenant = sqlx::query(
        r#"
        INSERT INTO model_task_profiles (
            id, tenant_id, model_id, taxonomy_version, task_kind,
            suitability_score, confidence, source
        ) VALUES ($1, $2, $3, 'task-taxonomy-v1', 'research', 80, 0.8000, 'benchmark')
        "#,
    )
    .bind(Uuid::from_u128(731))
    .bind(TENANT_A)
    .bind(model_b)
    .execute(&pool)
    .await;
    assert!(
        cross_tenant.is_err(),
        "cross-tenant model profile was accepted"
    );

    let invalid_task = sqlx::query(
        r#"
        INSERT INTO model_task_profiles (
            id, tenant_id, model_id, taxonomy_version, task_kind,
            suitability_score, confidence, source
        ) VALUES ($1, $2, $3, 'task-taxonomy-v1', 'similar_description', 80, 0.8000, 'benchmark')
        "#,
    )
    .bind(Uuid::from_u128(732))
    .bind(TENANT_A)
    .bind(model_a)
    .execute(&pool)
    .await;
    assert!(invalid_task.is_err(), "unknown task kind was accepted");
}

#[sqlx::test]
async fn classifier_config_and_request_axes_remain_independent(pool: PgPool) {
    let model_id = insert_model(&pool, TENANT_A, 740).await;
    sqlx::query(
        r#"
        INSERT INTO task_classifier_configs (
            id, tenant_id, taxonomy_version, classifier_model_id, mode
        ) VALUES ($1, $2, 'task-taxonomy-v1', $3, 'shadow')
        "#,
    )
    .bind(Uuid::from_u128(750))
    .bind(TENANT_A)
    .bind(model_id)
    .execute(&pool)
    .await
    .unwrap();

    let columns: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*) FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'gateway_requests'
          AND column_name = ANY($1)
        "#,
    )
    .bind(
        [
            "task_taxonomy_version",
            "classified_task",
            "classification_confidence",
            "capability_version",
            "required_capabilities",
            "service_objective_version",
            "service_objective",
        ]
        .as_slice(),
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(columns, 7);

    let disabled_without_model = sqlx::query(
        r#"
        INSERT INTO task_classifier_configs (
            id, tenant_id, taxonomy_version, mode
        ) VALUES ($1, $2, 'task-taxonomy-v1', 'disabled')
        "#,
    )
    .bind(Uuid::from_u128(751))
    .bind(TENANT_B)
    .execute(&pool)
    .await;
    assert!(disabled_without_model.is_ok());
}

#[sqlx::test]
async fn taxonomy_is_fixed_and_legacy_model_writes_still_work(pool: PgPool) {
    let mutation = sqlx::query(
        "UPDATE task_taxonomies SET fixture_sha256 = repeat('0', 64) WHERE taxonomy_version = 'task-taxonomy-v1'",
    )
    .execute(&pool)
    .await;
    assert!(mutation.is_err(), "released taxonomy could be rewritten");

    let model_id = insert_model(&pool, TENANT_A, 760).await;
    let version: i64 = sqlx::query_scalar("SELECT version FROM models WHERE id = $1")
        .bind(model_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        version, 1,
        "expand-only migration broke legacy model writes"
    );

    let first: i64 = sqlx::query_scalar("SELECT seed_model_task_profiles()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let second: i64 = sqlx::query_scalar("SELECT seed_model_task_profiles()")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(first, 1);
    assert_eq!(second, 0, "legacy conversion was not idempotent");

    let row = sqlx::query(
        r#"
        SELECT task_kind, suitability_score, source, lifecycle_state
        FROM model_task_profiles
        WHERE tenant_id = $1 AND model_id = $2
        "#,
    )
    .bind(TENANT_A)
    .bind(model_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("task_kind"), "general_assistance");
    assert_eq!(row.get::<i16, _>("suitability_score"), 0);
    assert_eq!(row.get::<String, _>("source"), "migration");
    assert_eq!(row.get::<String, _>("lifecycle_state"), "draft");
}
