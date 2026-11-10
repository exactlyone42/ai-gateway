use super::*;
use base64::Engine as _;
use std::os::unix::fs::PermissionsExt as _;

#[sqlx::test]
async fn codex_file_owner_is_enforced_by_the_responses_route(pool: PgPool) {
    use crate::identity::{NewCredential, NewPrincipal};
    use crate::model_registry::{ModelRegistry, ProviderProfile, ProviderProfiles};

    let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let captured = seen.clone();
    let upstream = Router::new().route(
        "/responses",
        post(move |headers: HeaderMap| {
            let captured = captured.clone();
            async move {
                captured.lock().unwrap().push((
                    headers["authorization"].to_str().unwrap().to_owned(),
                    headers["chatgpt-account-id"].to_str().unwrap().to_owned(),
                ));
                Json(json!({"id":"resp_owner","object":"response","status":"completed","model":"owner-model","output":[],"usage":{"input_tokens":1,"output_tokens":1}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let tenant = Uuid::new_v4();
    sqlx::query_file!(
        "tests/codex_owner_model_fixture.sql",
        Uuid::new_v4(),
        tenant,
        base_url,
        Uuid::new_v4()
    )
    .execute(&pool)
    .await
    .unwrap();
    let root = std::env::temp_dir().join(format!("codex-owner-http-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("auth.json");
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(json!({"exp": Utc::now().timestamp() + 3600}).to_string());
    let token = format!("header.{payload}.signature");
    std::fs::write(&path, json!({"auth_mode":"chatgpt","tokens":{"access_token":token,"account_id":"fixture-account"}}).to_string()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut config = test_config();
    config.codex_auth_file = Some(path.clone());
    config.codex_account_id = "fixture-account".into();
    config.provider_allowed_tenant_ids = vec![tenant];
    config.anthropic_personal_token_dir = Some(root.join("personal"));
    let spool = config.ledger_spool_dir.clone();
    let mut state = AppState::new(config, pool.clone()).unwrap();
    let mut keys = Vec::new();
    for name in ["owner", "other"] {
        let ids = state
            .credentials
            .create_principal(NewPrincipal {
                tenant_id: tenant,
                user_name: name,
                device_name: name,
                client_name: name,
                agent_name: name,
            })
            .await
            .unwrap();
        if name == "owner" {
            state.config.codex_owner_user_id = Some(ids.user_id);
        }
        keys.push(
            state
                .credentials
                .create(NewCredential {
                    tenant_id: tenant,
                    display_name: name,
                    principal: ids,
                    expires_at: None,
                })
                .await
                .unwrap()
                .plaintext,
        );
    }
    let mut profile =
        ProviderProfile::codex_file(path.clone(), "fixture-account".into(), vec![tenant]);
    profile.base_url = base_url;
    state.models = ModelRegistry::new(pool, ProviderProfiles::new([profile]).unwrap(), vec![]);
    let app = router(Arc::new(state));
    for (index, expected) in [
        (0, StatusCode::OK),
        (1, StatusCode::SERVICE_UNAVAILABLE),
        (0, StatusCode::SERVICE_UNAVAILABLE),
    ] {
        if index == 0 && expected == StatusCode::SERVICE_UNAVAILABLE {
            std::fs::remove_file(&path).unwrap();
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", format!("Bearer {}", keys[index]))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"owner-model","input":"fixture"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        assert_eq!(status, expected, "{}", String::from_utf8_lossy(&body));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(format!("Bearer {token}"), "fixture-account".into())]
    );
    drop(app);
    server.abort();
    let _ = server.await;
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(spool).unwrap();
}
