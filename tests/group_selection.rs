use sqlx::{PgPool, Row};
use uuid::Uuid;

async fn selected_with_model(pool: &PgPool, sql: &str) -> Vec<Uuid> {
    sqlx::query(sql)
        .bind(Uuid::from_u128(0x100))
        .bind(Some(Uuid::from_u128(1)))
        .bind(None::<Uuid>)
        .bind(None::<Uuid>)
        .bind(None::<Uuid>)
        .bind(None::<Uuid>)
        .bind(vec![Uuid::from_u128(0x3002)])
        .bind(vec![Uuid::from_u128(0x3001)])
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get("id"))
        .collect()
}

#[sqlx::test]
async fn model_match_does_not_select_other_group_quota(pool: PgPool) {
    sqlx::raw_sql(include_str!("group_selection_fixture.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("group_quota_model_fixture.sql"))
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query_file!("tests/group_quota_model_insert.sql")
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::raw_sql(include_str!("group_scope_drop_quota_check.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query_file!("tests/group_quota_model_insert.sql")
        .execute(&pool)
        .await
        .unwrap();
    let query = include_str!("../queries/quota/active.sql");
    assert!(
        !selected_with_model(&pool, query)
            .await
            .contains(&Uuid::from_u128(0x2012))
    );
    let broken = query.replace("q.group_id IS NULL AND q.user_id IS NULL AND q.device_id IS NULL\n            AND q.client_id IS NULL AND q.agent_id IS NULL AND q.credential_id IS NULL\n            AND (q.model_id = ANY($7) OR q.provider_id = ANY($8))", "q.model_id = ANY($7) OR q.provider_id = ANY($8)");
    assert_ne!(query, broken);
    assert!(
        selected_with_model(&pool, &broken)
            .await
            .contains(&Uuid::from_u128(0x2012))
    );
}

async fn selected(pool: &PgPool, sql: &str, quota: bool) -> Vec<Uuid> {
    let mut query = sqlx::query(sql)
        .bind(Uuid::from_u128(0x100))
        .bind(Some(Uuid::from_u128(1)))
        .bind(None::<Uuid>)
        .bind(None::<Uuid>)
        .bind(None::<Uuid>)
        .bind(None::<Uuid>);
    if quota {
        query = query.bind(Vec::<Uuid>::new()).bind(Vec::<Uuid>::new());
    }
    query
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get("id"))
        .collect()
}

#[sqlx::test]
async fn group_selection_respects_membership_and_personal_priority(pool: PgPool) {
    sqlx::raw_sql(include_str!("group_selection_fixture.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let policy = include_str!("../queries/policy/evaluate.sql");
    let quota = include_str!("../queries/quota/active.sql");
    assert_eq!(
        selected(&pool, policy, false).await,
        vec![Uuid::from_u128(0x1010), Uuid::from_u128(0x1011)]
    );
    assert_eq!(
        selected(&pool, quota, true).await,
        vec![Uuid::from_u128(0x2010), Uuid::from_u128(0x2011)]
    );
    sqlx::raw_sql(include_str!("group_selection_user_override.sql"))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        selected(&pool, policy, false).await,
        vec![
            Uuid::from_u128(0x1010),
            Uuid::from_u128(0x1011),
            Uuid::from_u128(0x1001)
        ]
    );
    let broken_policy = policy.replace(
        "AND gm.group_id = policies.group_id AND gm.user_id = $2\n                ))",
        "AND gm.group_id = policies.group_id AND gm.user_id = $2\n                ) AND NOT EXISTS (SELECT 1 FROM policies user_policy WHERE user_policy.tenant_id = policies.tenant_id AND user_policy.user_id = $2 AND user_policy.lifecycle_state = 'active'))",
    );
    assert_ne!(broken_policy, policy);
    assert_eq!(
        selected(&pool, &broken_policy, false).await,
        vec![Uuid::from_u128(0x1001)]
    );
    assert_eq!(
        selected(&pool, quota, true).await,
        vec![Uuid::from_u128(0x2001), Uuid::from_u128(0x2011)]
    );
    let broken_quota = quota.replace(
        "user_quota.user_id = $2",
        "user_quota.user_id = '00000000-0000-0000-0000-000000000999'::uuid",
    );
    assert_ne!(broken_quota, quota);
    assert_ne!(
        selected(&pool, &broken_quota, true).await,
        vec![Uuid::from_u128(0x2001), Uuid::from_u128(0x2011)]
    );
}

#[sqlx::test]
async fn personal_quota_must_cover_group_limit_and_mode(pool: PgPool) {
    sqlx::raw_sql(include_str!("group_selection_fixture.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("group_selection_user_override.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let quota = include_str!("../queries/quota/active.sql");
    for configuration in [
        serde_json::json!({"mode":"soft","limits":{"requests_per_minute":20}}),
        serde_json::json!({"mode":"hard","limits":{"tokens_per_hour":20}}),
        serde_json::json!({"mode":"hard","limits":{"requests_per_minute":null,"tokens_per_hour":20}}),
    ] {
        sqlx::query_file!("tests/group_quota_update_configuration.sql", configuration)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            selected(&pool, quota, true)
                .await
                .contains(&Uuid::from_u128(0x2010))
        );
    }
    let broken = quota.replace(
        "COALESCE(user_quota.configuration->'limits'->required.key, 'null'::jsonb) = 'null'::jsonb",
        "NOT (user_quota.configuration->'limits' ? required.key)",
    );
    assert_ne!(broken, quota);
    assert!(
        !selected(&pool, &broken, true)
            .await
            .contains(&Uuid::from_u128(0x2010))
    );
    sqlx::query_file!(
        "tests/group_quota_update_configuration.sql",
        serde_json::json!({"mode":"hard","limits":{"requests_per_minute":20}})
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        !selected(&pool, quota, true)
            .await
            .contains(&Uuid::from_u128(0x2010))
    );
}

#[sqlx::test]
async fn group_activation_only_disables_the_same_group(pool: PgPool) {
    sqlx::raw_sql(include_str!("group_selection_fixture.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("group_activation_fixture.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let tenant = Uuid::from_u128(0x100);
    for (query, target) in [
        (
            include_str!("../queries/admin/policy_activate.sql"),
            Uuid::from_u128(0x1012),
        ),
        (
            include_str!("../queries/admin/quota_activate.sql"),
            Uuid::from_u128(0x2012),
        ),
    ] {
        sqlx::query(query)
            .bind(tenant)
            .bind(target)
            .execute(&pool)
            .await
            .unwrap();
    }
    let policies: Vec<Uuid> = sqlx::query_file_scalar!("tests/group_active_ids.sql", tenant)
        .fetch_all(&pool)
        .await
        .unwrap();
    let quotas: Vec<Uuid> = sqlx::query_file_scalar!("tests/group_active_quota_ids.sql", tenant)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(policies, vec![Uuid::from_u128(0x1011)]);
    assert_eq!(quotas, vec![Uuid::from_u128(0x2011)]);
}

#[sqlx::test]
async fn missing_group_boundary_is_detected(pool: PgPool) {
    sqlx::raw_sql(include_str!("group_selection_fixture.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("group_activation_fixture.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let tenant = Uuid::from_u128(0x100);
    let boundary = "AND current.group_id IS NOT DISTINCT FROM target.group_id";
    for (query, target) in [
        (
            include_str!("../queries/admin/policy_activate.sql"),
            Uuid::from_u128(0x1012),
        ),
        (
            include_str!("../queries/admin/quota_activate.sql"),
            Uuid::from_u128(0x2012),
        ),
    ] {
        let broken = query.replace(boundary, "");
        assert_ne!(broken, query);
        sqlx::query(&broken)
            .bind(tenant)
            .bind(target)
            .execute(&pool)
            .await
            .unwrap();
    }
    let policies: Vec<Uuid> = sqlx::query_file_scalar!("tests/group_active_ids.sql", tenant)
        .fetch_all(&pool)
        .await
        .unwrap();
    let quotas: Vec<Uuid> = sqlx::query_file_scalar!("tests/group_active_quota_ids.sql", tenant)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_ne!(policies, vec![Uuid::from_u128(0x1011)]);
    assert_ne!(quotas, vec![Uuid::from_u128(0x2011)]);
}
