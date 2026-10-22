use sqlx::PgPool;
use uuid::Uuid;

const TENANT: Uuid = Uuid::from_u128(950);

async fn insert_decomposition(pool: &PgPool, orchestration_id: Uuid, reason: &str, nodes: i32) {
    sqlx::query(
        "INSERT INTO task_decompositions
            (tenant_id, orchestration_id, gateway_request_id, decision, reason,
             node_count, max_depth, elapsed_us, contract_version)
         VALUES ($1, $2, $3, CASE WHEN $5 = 0 THEN 'direct' ELSE 'decomposed' END,
                 $4, $5, LEAST($5, 1), 10, 'task-graph-v1')",
    )
    .bind(TENANT)
    .bind(orchestration_id)
    .bind(Uuid::new_v4())
    .bind(reason)
    .bind(nodes)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn a_new_tenant_defaults_to_disabled_and_local_only(pool: PgPool) {
    sqlx::query("INSERT INTO orchestration_configs (tenant_id) VALUES ($1)")
        .bind(TENANT)
        .execute(&pool)
        .await
        .unwrap();
    let (mode, policy): (String, String) =
        sqlx::query_as("SELECT mode, data_policy FROM orchestration_configs")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((mode.as_str(), policy.as_str()), ("disabled", "local_only"));
    for bad in ["enabled", "active"] {
        let result = sqlx::query("UPDATE orchestration_configs SET mode = $1")
            .bind(bad)
            .execute(&pool)
            .await;
        assert!(result.is_err(), "{bad} accepted");
    }
}

#[sqlx::test]
async fn decompositions_reject_free_text_reasons_and_inconsistent_counts(pool: PgPool) {
    insert_decomposition(&pool, Uuid::from_u128(1), "bad_json", 0).await;
    insert_decomposition(&pool, Uuid::from_u128(2), "decomposed", 2).await;
    let bad_reason = sqlx::query(
        "INSERT INTO task_decompositions
            (tenant_id, orchestration_id, gateway_request_id, decision, reason,
             node_count, max_depth, elapsed_us, contract_version)
         VALUES ($1, $2, $3, 'direct', 'Write me a poem about the secret', 0, 0, 1, 'v1')",
    )
    .bind(TENANT)
    .bind(Uuid::from_u128(3))
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await;
    assert!(bad_reason.is_err());
    let direct_with_nodes = sqlx::query(
        "INSERT INTO task_decompositions
            (tenant_id, orchestration_id, gateway_request_id, decision, reason,
             node_count, max_depth, elapsed_us, contract_version)
         VALUES ($1, $2, $3, 'direct', 'model_direct', 2, 1, 1, 'v1')",
    )
    .bind(TENANT)
    .bind(Uuid::from_u128(4))
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await;
    assert!(direct_with_nodes.is_err());
    let duplicate = sqlx::query(
        "INSERT INTO task_decompositions
            (tenant_id, orchestration_id, gateway_request_id, decision, reason,
             node_count, max_depth, elapsed_us, contract_version)
         VALUES ($1, $2, $3, 'direct', 'bad_json', 0, 0, 1, 'v1')",
    )
    .bind(TENANT)
    .bind(Uuid::from_u128(1))
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await;
    assert!(
        duplicate.is_err(),
        "orchestration id must be unique per tenant"
    );
}

#[sqlx::test]
async fn decomposition_rows_are_append_only(pool: PgPool) {
    insert_decomposition(&pool, Uuid::from_u128(10), "bad_json", 0).await;
    assert!(
        sqlx::query("UPDATE task_decompositions SET reason = 'other'")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM task_decompositions")
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test]
async fn execution_receipts_are_unique_per_role_task_and_attempt(pool: PgPool) {
    let insert = |role: &'static str, task: &'static str, attempt: i32| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO orchestration_executions
                    (tenant_id, orchestration_id, execution_role, task_id, attempt, status, elapsed_us)
                 VALUES ($1, $2, $3, $4, $5, 'completed', 1)",
            )
            .bind(TENANT)
            .bind(Uuid::from_u128(20))
            .bind(role)
            .bind(task)
            .bind(attempt)
            .execute(&pool)
            .await
        }
    };
    insert("decomposer", "__decomposer__", 1).await.unwrap();
    insert("worker", "__decomposer__", 1).await.unwrap();
    insert("decomposer", "__decomposer__", 2).await.unwrap();
    assert!(insert("decomposer", "__decomposer__", 1).await.is_err());
    assert!(insert("reviewer", "t1", 1).await.is_err());
}

#[sqlx::test]
async fn no_column_can_hold_prompt_or_objective_text(pool: PgPool) {
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns
         WHERE table_name IN ('task_decompositions', 'orchestration_executions', 'orchestration_configs')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(!columns.is_empty());
    for banned in [
        "prompt",
        "objective",
        "content",
        "output",
        "body",
        "message",
    ] {
        assert!(
            columns.iter().all(|name| !name.contains(banned)),
            "column containing {banned}: {columns:?}"
        );
    }
}
