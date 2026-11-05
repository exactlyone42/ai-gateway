use crate::admin::{AdminContext, AdminRole, GroupCredentialKind, GroupCredentialTarget};
use crate::group_credentials::*;
use crate::held_token::PersonalTokens;
use crate::state::AppState;
use sqlx::PgPool;
use uuid::Uuid;

const PROVIDER: &str = "anthropic-test";

fn tenant() -> Uuid {
    Uuid::from_u128(0x100)
}
fn user() -> Uuid {
    Uuid::from_u128(1)
}
fn group() -> Uuid {
    Uuid::from_u128(0x10)
}
fn context() -> AdminContext {
    AdminContext {
        tenant_id: tenant(),
        account_id: Uuid::from_u128(0x99),
        session_id: Uuid::new_v4(),
        role: AdminRole::Admin,
    }
}
fn target(group: Uuid) -> GroupCredentialTarget {
    GroupCredentialTarget {
        group,
        provider: PROVIDER.into(),
        kind: GroupCredentialKind::Subscription,
    }
}
async fn setup(pool: PgPool) -> (AppState, std::path::PathBuf) {
    sqlx::raw_sql(include_str!("../../../tests/group_credentials_fixture.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let root = std::env::temp_dir().join(format!("group-credentials-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let mut config = crate::routes::tests::test_config();
    config.anthropic_personal_token_dir = Some(root.clone());
    (AppState::new(config, pool).unwrap(), root)
}
async fn change(
    state: &AppState,
    group: Uuid,
    token: Option<&str>,
    request: &str,
) -> Result<(), crate::admin::AdminError> {
    let ip = "127.0.0.1".parse().unwrap();
    state
        .admin
        .change_group_credential(
            &context(),
            target(group),
            state.personal_tokens.as_ref().unwrap(),
            token,
            (ip, ip),
            request,
        )
        .await
}

#[tokio::test]
async fn group_files_are_separate_from_users_providers_and_api_keys() {
    let root = std::env::temp_dir().join(format!("group-namespaces-{}", Uuid::new_v4()));
    let store = PersonalTokens::new(root.clone());
    store
        .replace(tenant(), group(), "sk-ant-oat01-user")
        .await
        .unwrap();
    let groups = store.group_store(PROVIDER);
    groups
        .replace(tenant(), group(), "sk-ant-oat01-group")
        .await
        .unwrap();
    assert_eq!(
        &*store.get(tenant(), group()).await.unwrap().unwrap(),
        "sk-ant-oat01-user"
    );
    assert_eq!(
        &*groups.get(tenant(), group()).await.unwrap().unwrap(),
        "sk-ant-oat01-group"
    );
    assert!(
        store
            .group_store("other")
            .get(tenant(), group())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .api_store(PROVIDER)
            .group_store(PROVIDER)
            .get(tenant(), group())
            .await
            .unwrap()
            .is_none()
    );
    let (missing, lock) = store.read_locked(tenant(), user()).await.unwrap();
    assert!(missing.is_none());
    assert!(
        store
            .replace(tenant(), user(), "sk-ant-oat01-new")
            .await
            .is_err()
    );
    lock.unlock().unwrap();
    store
        .replace(tenant(), user(), "sk-ant-oat01-new")
        .await
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[sqlx::test]
async fn group_selection_is_scoped_unambiguous_and_personal_errors_never_fall_back(pool: PgPool) {
    let (state, root) = setup(pool.clone()).await;
    let store = state.personal_tokens.as_ref().unwrap();
    assert!(
        select(&pool, store, tenant(), user(), PROVIDER, "subscription")
            .await
            .is_err()
    );
    change(&state, group(), Some("sk-ant-oat01-group"), "first")
        .await
        .unwrap();
    for (t, u, p) in [
        (Uuid::new_v4(), user(), PROVIDER),
        (tenant(), Uuid::from_u128(2), PROVIDER),
        (tenant(), user(), "other"),
    ] {
        assert!(select(&pool, store, t, u, p, "subscription").await.is_err());
    }
    let (token, lease) = select(&pool, store, tenant(), user(), PROVIDER, "subscription")
        .await
        .unwrap();
    assert_eq!(&*token, "sk-ant-oat01-group");
    assert!(
        store
            .replace(tenant(), user(), "sk-ant-oat01-personal")
            .await
            .is_err()
    );
    drop(lease);
    change(
        &state,
        Uuid::from_u128(0x11),
        Some("sk-ant-oat01-second"),
        "second",
    )
    .await
    .unwrap();
    assert!(
        select(&pool, store, tenant(), user(), PROVIDER, "subscription")
            .await
            .is_err()
    );
    store
        .replace(tenant(), user(), "sk-ant-oat01-personal")
        .await
        .unwrap();
    let (token, lease) = select(&pool, store, tenant(), user(), PROVIDER, "subscription")
        .await
        .unwrap();
    assert_eq!(&*token, "sk-ant-oat01-personal");
    drop(lease);
    std::fs::write(
        root.join(tenant().to_string()).join(user().to_string()),
        "corrupt",
    )
    .unwrap();
    assert!(
        select(&pool, store, tenant(), user(), PROVIDER, "subscription")
            .await
            .is_err()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[sqlx::test]
async fn group_lease_blocks_member_removal_and_token_change_until_dispatch_finishes(pool: PgPool) {
    let (state, root) = setup(pool.clone()).await;
    let store = state.personal_tokens.as_ref().unwrap();
    change(&state, group(), Some("sk-ant-oat01-group"), "first")
        .await
        .unwrap();
    let ip = "127.0.0.1".parse().unwrap();
    assert!(
        state
            .admin
            .change_group_member(
                &context(),
                (group(), user()),
                false,
                None,
                (ip, ip),
                "unconfigured-member"
            )
            .await
            .is_err()
    );
    let (_, lease) = select(&pool, store, tenant(), user(), PROVIDER, "subscription")
        .await
        .unwrap();
    let mut removal = pool.begin().await.unwrap();
    sqlx::query_file!("tests/group_credentials_short_lock_timeout.sql")
        .execute(&mut *removal)
        .await
        .unwrap();
    let error = sqlx::query_file_scalar!(
        "queries/admin/group_member_remove.sql",
        tenant(),
        group(),
        user()
    )
    .fetch_optional(&mut *removal)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("55P03")
    );
    removal.rollback().await.unwrap();
    assert!(change(&state, group(), None, "busy").await.is_err());
    let ip = "127.0.0.1".parse().unwrap();
    assert!(
        state
            .admin
            .change_group_member(
                &context(),
                (group(), user()),
                false,
                Some(store),
                (ip, ip),
                "busy-member"
            )
            .await
            .is_err()
    );
    drop(lease);
    state
        .admin
        .change_group_member(
            &context(),
            (group(), user()),
            false,
            Some(store),
            (ip, ip),
            "remove-member",
        )
        .await
        .unwrap();
    assert!(
        select(&pool, store, tenant(), user(), PROVIDER, "subscription")
            .await
            .is_err()
    );
    change(&state, group(), None, "revoked").await.unwrap();
    let status = state
        .admin
        .group_credential_status(&context(), target(group()), store)
        .await
        .unwrap();
    assert_eq!(status["configured"], false);
    std::fs::remove_dir_all(root).unwrap();
}

#[sqlx::test]
async fn group_file_change_without_committed_audit_is_rejected_and_reported_inconsistent(
    pool: PgPool,
) {
    let (state, root) = setup(pool.clone()).await;
    let store = state.personal_tokens.as_ref().unwrap();
    change(&state, group(), Some("sk-ant-oat01-before"), "first")
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../tests/group_credentials_reject_success_audit.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        change(
            &state,
            group(),
            Some("sk-ant-oat01-after"),
            "group-audit-failure"
        )
        .await,
        Err(crate::admin::AdminError::OperationUncertain)
    ));
    assert!(
        select(&pool, store, tenant(), user(), PROVIDER, "subscription")
            .await
            .is_err()
    );
    let status = state
        .admin
        .group_credential_status(&context(), target(group()), store)
        .await
        .unwrap();
    assert_eq!(status["consistent"], false);
    let count = sqlx::query_file_scalar!(
        "tests/group_credentials_audit_count.sql",
        "group-audit-failure",
        "admin.group_credential.replace_failure"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    change(&state, group(), Some("sk-ant-oat01-repaired"), "repair")
        .await
        .unwrap();
    let (token, lease) = select(&pool, store, tenant(), user(), PROVIDER, "subscription")
        .await
        .unwrap();
    assert_eq!(&*token, "sk-ant-oat01-repaired");
    drop(lease);
    std::fs::remove_dir_all(root).unwrap();
}

#[sqlx::test]
async fn group_management_and_authenticated_requests_use_only_the_bound_credential(pool: PgPool) {
    use crate::{
        admin::AdminAccountCreate,
        identity::{NewCredential, NewPrincipal},
    };
    use axum::{
        Json, Router,
        body::Body,
        extract::ConnectInfo,
        http::{Request, StatusCode},
        routing::post,
    };
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = seen.clone();
    let upstream = Router::new().route("/v1/messages", post(move |headers: axum::http::HeaderMap| {
        let captured = captured.clone();
        async move {
            captured.lock().unwrap().push(headers["authorization"].to_str().unwrap().into());
            Json(json!({"id":"msg_test","type":"message","role":"assistant","model":"upstream-model","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let (initial, root) = setup(pool.clone()).await;
    drop(initial);
    sqlx::query_file!("tests/group_credentials_model.sql", &base_url, tenant())
        .execute(&pool)
        .await
        .unwrap();
    let mut config = crate::routes::tests::test_config();
    config.anthropic_base_url = base_url;
    config.anthropic_subscription_token_file = Some(root.join("legacy"));
    config.anthropic_personal_token_dir = Some(root.clone());
    config.provider_allowed_tenant_ids = vec![tenant()];
    let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
    let peer: std::net::SocketAddr = "127.0.0.1:4567".parse().unwrap();
    let mut sessions = Vec::new();
    for role in [AdminRole::Admin, AdminRole::Operator, AdminRole::Viewer] {
        let account = state
            .admin
            .create_admin_account(
                &context(),
                AdminAccountCreate {
                    display_name: role.as_str().to_owned(),
                    credential_name: "login".into(),
                    role,
                },
                peer.ip(),
                peer.ip(),
                "create-account",
            )
            .await
            .unwrap();
        sessions.push(
            state
                .admin
                .login(&account.login_token, peer.ip(), peer.ip(), "login")
                .await
                .unwrap()
                .access_token,
        );
    }
    let identity = state
        .credentials
        .create_principal(NewPrincipal {
            tenant_id: tenant(),
            user_name: "member",
            device_name: "device",
            client_name: "client",
            agent_name: "agent",
        })
        .await
        .unwrap();
    state
        .admin
        .change_group_member(
            &context(),
            (group(), identity.user_id),
            true,
            state.personal_tokens.as_ref(),
            (peer.ip(), peer.ip()),
            "member",
        )
        .await
        .unwrap();
    let key = state
        .credentials
        .create(NewCredential {
            tenant_id: tenant(),
            display_name: "member",
            principal: identity,
            expires_at: None,
        })
        .await
        .unwrap()
        .plaintext;
    let app = crate::routes::router(state.clone());
    let admin_request = |method: &str, group_id: Uuid, session: &str| {
        let mut request = Request::builder()
            .method(method)
            .uri(format!(
                "/admin/v1/groups/{group_id}/provider-credentials/{PROVIDER}/subscription"
            ))
            .header("authorization", format!("Bearer {session}"))
            .header("content-type", "application/json")
            .header("x-request-id", Uuid::new_v4().to_string())
            .body(if method == "PUT" {
                Body::from(r#"{"token":"sk-ant-oat01-group-http"}"#)
            } else {
                Body::empty()
            })
            .unwrap();
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    };
    for session in &sessions[1..] {
        assert_eq!(
            app.clone()
                .oneshot(admin_request("PUT", group(), session))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        app.clone()
            .oneshot(admin_request("PUT", Uuid::new_v4(), &sessions[0]))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.clone()
            .oneshot(admin_request("PUT", group(), &sessions[0]))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let status = app
        .clone()
        .oneshot(admin_request("GET", group(), &sessions[0]))
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    let body = axum::body::to_bytes(status.into_body(), 4096)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("sk-ant-oat"));
    for expected in [StatusCode::OK, StatusCode::SERVICE_UNAVAILABLE] {
        let response = app.clone().oneshot(Request::builder().method("POST").uri("/v1/messages")
            .header("x-api-key",&key).header("content-type","application/json")
            .body(Body::from(r#"{"model":"group-model","max_tokens":8,"messages":[{"role":"user","content":"hello"}]}"#)).unwrap()).await.unwrap();
        assert_eq!(response.status(), expected);
        axum::body::to_bytes(response.into_body(), 16384)
            .await
            .unwrap();
        if expected == StatusCode::OK {
            assert_eq!(
                app.clone()
                    .oneshot(admin_request("DELETE", group(), &sessions[0]))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NO_CONTENT
            );
        }
    }
    assert_eq!(*seen.lock().unwrap(), ["Bearer sk-ant-oat01-group-http"]);
    server.abort();
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[sqlx::test]
async fn group_api_credentials_are_separate_from_subscription_bindings(pool: PgPool) {
    let (state, root) = setup(pool.clone()).await;
    let store = state.personal_tokens.as_ref().unwrap();
    let ip = "127.0.0.1".parse().unwrap();
    let api_target = GroupCredentialTarget {
        group: group(),
        provider: PROVIDER.into(),
        kind: GroupCredentialKind::Api,
    };
    state
        .admin
        .change_group_credential(
            &context(),
            api_target,
            store,
            Some("group-api-key"),
            (ip, ip),
            "api",
        )
        .await
        .unwrap();
    assert!(
        select(&pool, store, tenant(), user(), PROVIDER, "subscription")
            .await
            .is_err()
    );
    let (token, lease) = select(
        &pool,
        &store.api_store(PROVIDER),
        tenant(),
        user(),
        PROVIDER,
        "api",
    )
    .await
    .unwrap();
    assert_eq!(&*token, "group-api-key");
    assert!(
        store
            .lock_membership(tenant(), user(), false)
            .await
            .is_err()
    );
    drop(lease);
    let error = sqlx::query_file!(
        "queries/admin/group_credential_set.sql",
        Uuid::new_v4(),
        group(),
        PROVIDER,
        "api",
        vec![0u8; 32]
    )
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23503")
    );
    let error = sqlx::query_file!(
        "queries/admin/group_credential_set.sql",
        tenant(),
        group(),
        PROVIDER,
        "api",
        vec![0u8; 6]
    )
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23514")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[sqlx::test]
async fn group_attempt_records_failure_when_subject_changes_before_file_write(pool: PgPool) {
    let (state, root) = setup(pool.clone()).await;
    sqlx::raw_sql(include_str!(
        "../../../tests/group_credentials_archive_after_attempt.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        change(
            &state,
            group(),
            Some("sk-ant-oat01-unused"),
            "subject-changed"
        )
        .await,
        Err(crate::admin::AdminError::NotFound)
    ));
    let count = sqlx::query_file_scalar!(
        "tests/group_credentials_audit_count.sql",
        "subject-changed",
        "admin.group_credential.replace_failure"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    assert!(
        state
            .personal_tokens
            .as_ref()
            .unwrap()
            .group_store(PROVIDER)
            .get(tenant(), group())
            .await
            .unwrap()
            .is_none()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[sqlx::test]
async fn group_member_removal_rechecks_binding_after_waiting_for_writer(pool: PgPool) {
    let (state, root) = setup(pool.clone()).await;
    let state = std::sync::Arc::new(state);
    let mut writer = pool.begin().await.unwrap();
    sqlx::query_file_scalar!(
        "queries/admin/group_credential_subject.sql",
        tenant(),
        group(),
        PROVIDER,
        "subscription"
    )
    .fetch_one(&mut *writer)
    .await
    .unwrap();
    let digest = vec![1u8; 32];
    sqlx::query_file!(
        "queries/admin/group_credential_set.sql",
        tenant(),
        group(),
        PROVIDER,
        "subscription",
        digest
    )
    .execute(&mut *writer)
    .await
    .unwrap();
    let removing = state.clone();
    let removal = tokio::spawn(async move {
        let ip = "127.0.0.1".parse().unwrap();
        removing
            .admin
            .change_group_member(
                &context(),
                (group(), user()),
                false,
                None,
                (ip, ip),
                "waiting-removal",
            )
            .await
    });
    let mut waiting = false;
    for _ in 0..40 {
        if sqlx::query_file_scalar!("tests/group_credentials_waiting_member.sql")
            .fetch_one(&pool)
            .await
            .unwrap()
        {
            waiting = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(waiting);
    writer.commit().await.unwrap();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), removal)
            .await
            .unwrap()
            .unwrap(),
        Err(crate::admin::AdminError::Unavailable)
    ));
    let members = state
        .admin
        .list_group_members(&context(), group(), serde_json::from_str("{}").unwrap())
        .await
        .unwrap();
    assert_eq!(members.items.len(), 1);
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}
