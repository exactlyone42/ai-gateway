use sqlx::PgPool;
use uuid::Uuid;

const TENANT_A: Uuid = Uuid::from_u128(100);
const TENANT_B: Uuid = Uuid::from_u128(200);

async fn user(
    pool: &PgPool,
    id: u128,
    tenant: Uuid,
    email: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO users (id, tenant_id, display_name, email) VALUES ($1, $2, 'u', $3)")
        .bind(Uuid::from_u128(id))
        .bind(tenant)
        .bind(email)
        .execute(pool)
        .await
        .map(|_| ())
}

async fn group(pool: &PgPool, id: u128, tenant: Uuid, name: &str) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO groups (id, tenant_id, display_name) VALUES ($1, $2, $3)")
        .bind(Uuid::from_u128(id))
        .bind(tenant)
        .bind(name)
        .execute(pool)
        .await
        .map(|_| ())
}

async fn member(pool: &PgPool, tenant: Uuid, group: u128, user: u128) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO group_members (tenant_id, group_id, user_id) VALUES ($1, $2, $3)")
        .bind(tenant)
        .bind(Uuid::from_u128(group))
        .bind(Uuid::from_u128(user))
        .execute(pool)
        .await
        .map(|_| ())
}

#[sqlx::test]
async fn email_is_optional_and_unique_per_tenant_ignoring_case(pool: PgPool) {
    user(&pool, 1, TENANT_A, None).await.unwrap();
    user(&pool, 2, TENANT_A, None).await.unwrap();
    user(&pool, 3, TENANT_A, Some("Cash@Example.com"))
        .await
        .unwrap();
    assert!(
        user(&pool, 4, TENANT_A, Some("cash@example.com"))
            .await
            .is_err(),
        "duplicate email in one tenant was accepted"
    );
    user(&pool, 5, TENANT_B, Some("cash@example.com"))
        .await
        .unwrap();
    assert!(
        user(&pool, 6, TENANT_A, Some("  ")).await.is_err(),
        "blank email was accepted"
    );
}

#[sqlx::test]
async fn duplicate_email_check_detects_a_missing_unique_index(pool: PgPool) {
    user(&pool, 1, TENANT_A, Some("Cash@Example.com"))
        .await
        .unwrap();
    assert!(
        user(&pool, 2, TENANT_A, Some("cash@example.com"))
            .await
            .is_err()
    );
    sqlx::raw_sql(include_str!("drop_email_uniqueness.sql"))
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        user(&pool, 2, TENANT_A, Some("cash@example.com"))
            .await
            .is_ok()
    );
}

#[sqlx::test]
async fn group_members_stay_inside_one_tenant_and_are_unique(pool: PgPool) {
    user(&pool, 1, TENANT_A, None).await.unwrap();
    user(&pool, 2, TENANT_B, None).await.unwrap();
    group(&pool, 10, TENANT_A, "team").await.unwrap();
    assert!(
        group(&pool, 11, TENANT_A, "TEAM").await.is_err(),
        "duplicate group name in one tenant was accepted"
    );
    group(&pool, 12, TENANT_B, "team").await.unwrap();

    member(&pool, TENANT_A, 10, 1).await.unwrap();
    assert!(
        member(&pool, TENANT_A, 10, 1).await.is_err(),
        "same member was added twice"
    );
    assert!(
        member(&pool, TENANT_A, 10, 2).await.is_err(),
        "a user from another tenant joined the group"
    );
    assert!(
        member(&pool, TENANT_B, 10, 2).await.is_err(),
        "a group from another tenant accepted a member"
    );
}

#[sqlx::test]
async fn cross_tenant_member_check_detects_missing_foreign_key(pool: PgPool) {
    user(&pool, 2, TENANT_B, None).await.unwrap();
    group(&pool, 10, TENANT_A, "team").await.unwrap();
    assert!(member(&pool, TENANT_A, 10, 2).await.is_err());
    sqlx::raw_sql(include_str!("drop_group_member_user_fk.sql"))
        .execute(&pool)
        .await
        .unwrap();
    assert!(member(&pool, TENANT_A, 10, 2).await.is_ok());
}

#[sqlx::test]
async fn groups_and_memberships_cannot_be_deleted_with_members_hidden(pool: PgPool) {
    user(&pool, 1, TENANT_A, None).await.unwrap();
    group(&pool, 10, TENANT_A, "team").await.unwrap();
    member(&pool, TENANT_A, 10, 1).await.unwrap();
    let removed = sqlx::query("DELETE FROM groups WHERE id = $1")
        .bind(Uuid::from_u128(10))
        .execute(&pool)
        .await;
    assert!(removed.is_err(), "group with members was deleted");
}
