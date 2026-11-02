use sqlx::PgPool;

async fn verify_guard(pool: PgPool, mutation: &str, expected: &str) {
    let checks = include_str!("group_scope_constraints.sql");
    sqlx::raw_sql(checks).execute(&pool).await.unwrap();
    sqlx::raw_sql(mutation).execute(&pool).await.unwrap();
    let error = sqlx::raw_sql(checks).execute(&pool).await.unwrap_err();
    assert!(error.to_string().contains(expected));
}

macro_rules! guard_test {
    ($name:ident, $file:literal, $expected:literal) => {
        #[sqlx::test]
        async fn $name(pool: PgPool) {
            verify_guard(pool, include_str!($file), $expected).await;
        }
    };
}

guard_test!(
    policy_tenant_guard,
    "group_scope_drop_policy_fk.sql",
    "cross tenant policy accepted"
);
guard_test!(
    quota_tenant_guard,
    "group_scope_drop_quota_fk.sql",
    "cross tenant quota accepted"
);
guard_test!(
    policy_scope_guard,
    "group_scope_drop_policy_check.sql",
    "mixed policy accepted"
);
guard_test!(
    quota_scope_guard,
    "group_scope_drop_quota_check.sql",
    "mixed quota accepted"
);
guard_test!(
    policy_uniqueness_guard,
    "group_scope_drop_policy_index.sql",
    "duplicate policy accepted"
);
guard_test!(
    quota_uniqueness_guard,
    "group_scope_drop_quota_index.sql",
    "duplicate quota accepted"
);
