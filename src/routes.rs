mod admin;
mod capture;
mod chat_completions;
mod health;
mod messages;
mod responses;

pub(crate) use messages::StreamingResponse;

use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, rejection::BytesRejection},
    http::{HeaderName, HeaderValue, header},
    routing::{delete, get, head, post},
};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
};

use crate::{
    middleware::{REQUEST_ID_HEADER, access_log, process_gate, require_client_key},
    state::AppState,
};

/// The route table: which endpoints exist, which of them need a client key,
/// and the layers every request crosses.
pub fn router(state: Arc<AppState>) -> Router {
    let request_id_header = HeaderName::from_static(REQUEST_ID_HEADER);
    let openai = Router::new()
        .route("/v1/chat/completions", post(chat_completions::handle))
        .route("/v1/responses", post(responses::handle))
        .layer(DefaultBodyLimit::max(state.config.openai_request_max_bytes));
    let anthropic = Router::new()
        .route("/v1/messages", post(messages::messages))
        .route("/v1/messages/count_tokens", post(messages::count_tokens))
        .layer(DefaultBodyLimit::max(
            state.config.anthropic_request_max_bytes,
        ));
    let authenticated = Router::new()
        .merge(openai)
        .merge(anthropic)
        .route("/v1/models", get(messages::models))
        .route("/v1/request-captures/{id}", delete(capture::delete))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_client_key,
        ));
    let admin = admin::router(&state);
    let admin_ui = Router::new()
        .nest_service(
            "/admin",
            ServeDir::new("web/dist")
                .append_index_html_on_directories(true)
                .fallback(ServeFile::new("web/dist/index.html")),
        )
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; font-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
            ),
        ));

    Router::new()
        // `/healthz` is the name the old single endpoint had. Keeping it as
        // an alias means existing callers do not break on this change.
        .route("/healthz", get(health::live))
        .route("/health", get(health::live))
        .route("/ready", get(health::ready))
        .route("/api/hello", head(messages::hello))
        .merge(authenticated)
        .merge(admin)
        .merge(admin_ui)
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            process_gate,
        ))
        // Innermost first: the access log needs the id already set and the
        // status already final, so it sits inside the two id layers and
        // outside the handlers. It replaces `TraceLayer`, which reported the
        // same requests without any of the fields a consumer filters on.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            access_log,
        ))
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
}

pub(crate) type BodyBytes = Result<Bytes, BytesRejection>;

pub(crate) fn body_bytes(body: BodyBytes) -> Result<Bytes, crate::error::GatewayError> {
    body.map_err(|error| {
        if error.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
            crate::error::GatewayError::PayloadTooLarge
        } else {
            crate::error::GatewayError::RequestBodyUnavailable(error.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    mod codex_owner;
    mod group_credentials;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::time::Duration;

    use axum::{
        Json, Router,
        body::{Body, Bytes, to_bytes},
        extract::OriginalUri,
        http::{HeaderMap, Request, StatusCode},
        response::{IntoResponse as _, Response},
        routing::post,
    };
    use futures_util::{StreamExt as _, TryStreamExt as _, stream};
    use serde_json::json;
    use sqlx::{PgPool, Row as _};
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        sync::Notify,
    };
    use tower::ServiceExt as _;
    use tracing::instrument::WithSubscriber as _;
    use uuid::Uuid;

    use super::router;
    use chrono::Utc;

    use crate::{
        config::{Config, ExtraProviderProfile},
        db,
        identity::PrincipalContext,
        ledger::{Ledger, RequestCompletion, RequestStart},
        logging,
        model_registry::{GatewayOperation, ProviderKind},
        services::openai::{self, Operation as OpenAiOperation, RequestContext},
        startup,
        state::AppState,
    };

    pub(super) fn test_config() -> Config {
        Config {
            listen_address: "127.0.0.1:0".parse().unwrap(),
            openai_base_url: "http://127.0.0.1:1".to_owned(),
            openai_api_key: "upstream-key".to_owned(),
            codex_auth_file: None,
            codex_account_id: String::new(),
            codex_owner_user_id: None,
            openai_provider: "openai-test".to_owned(),
            openai_request_max_bytes: 32 * 1024 * 1024,
            anthropic_base_url: "http://127.0.0.1:1".to_owned(),
            anthropic_api_key: "provider-key".to_owned(),
            anthropic_auth_token: None,
            anthropic_subscription_token_file: None,
            anthropic_personal_token_dir: None,
            anthropic_subscription_token_seed: None,
            anthropic_client_oauth: false,
            anthropic_oauth_credential_id: None,
            anthropic_provider: "anthropic-test".to_owned(),
            anthropic_request_max_bytes: 32 * 1024 * 1024,
            extra_provider_profiles: Vec::new(),
            stream_event_max_bytes: 32 * 1024 * 1024,
            stream_heartbeat_interval: Duration::from_secs(30),
            provider_idle_timeout: Duration::from_secs(120),
            local_provider_idle_timeout: Duration::from_secs(300),
            provider_connect_timeout: Duration::from_secs(10),
            request_header_max_bytes: 64 * 1024,
            drain_timeout: Duration::from_secs(30),
            shutdown_finalize_timeout: Duration::from_secs(5),
            bootstrap_client_key: Some("client-key".to_owned()),
            allowed_models: vec!["company-default".to_owned()],
            otlp_endpoint: None,
            // Port 1 has no listener. The pool is lazy, so building it costs
            // nothing; any test that actually queries would fail, which is
            // what we want -- these tests must not depend on a live database.
            database_url: "postgres://gateway@127.0.0.1:1/gateway".to_owned(),
            database_max_connections: 1,
            admin_database_max_connections: 1,
            credential_hash_key: b"test-credential-hash-key-at-least-32".to_vec(),
            credential_hash_key_id: "v1".to_owned(),
            credential_retiring_hash_keys: Vec::new(),
            credential_environment: "dev".to_owned(),
            credential_cache_capacity: 8,
            credential_cache_ttl_seconds: 60,
            policy_cache_capacity: 8,
            policy_cache_ttl_seconds: 60,
            admin_bootstrap_token: None,
            admin_bootstrap_tenant_id: None,
            provider_allowed_tenant_ids: vec![Uuid::nil()],
            admin_session_ttl_seconds: 900,
            admin_allowed_origin: None,
            admin_trusted_proxy_ips: Vec::new(),
            ledger_spool_dir: std::env::temp_dir().join(format!(
                "agent-pool-gateway-route-test-{}",
                uuid::Uuid::new_v4()
            )),
            ledger_spool_max_bytes: 1024 * 1024,
            ledger_spool_entry_max_bytes: 32 * 1024,
            content_capture_enabled: false,
            content_capture_spool_dir: "/var/lib/agent-pool-gateway/capture-spool".into(),
            content_capture_key_dir: "/var/lib/agent-pool-gateway/capture-keys".into(),
            content_capture_anchor_dir: "/var/lib/agent-pool-gateway/capture-anchors".into(),
            content_capture_erasure_dir: "/var/lib/agent-pool-gateway/capture-erasure".into(),
            content_capture_witness_dir: "/var/lib/agent-pool-gateway/capture-witness".into(),
            content_capture_remote_witness_url: None,
            content_capture_test_allow_local_witness: false,
            routing: Default::default(),
            jev: None,
            scheduler: Default::default(),
        }
    }

    /// Builds the state these tests share: real router, no live database.
    fn test_state() -> Arc<AppState> {
        let config = test_config();
        let pool = db::pool(&config.database_url, config.database_max_connections)
            .expect("the test connection string is valid");
        Arc::new(AppState::new(config, pool).unwrap())
    }

    fn principal(tenant_id: Uuid) -> PrincipalContext {
        PrincipalContext {
            tenant_id,
            credential_id: Uuid::nil(),
            credential_name: "route-test".to_owned(),
            user_id: None,
            user_name: None,
            device_id: None,
            device_name: None,
            client_id: None,
            client_name: None,
            agent_id: None,
            agent_name: None,
        }
    }

    #[sqlx::test]
    async fn personal_api_key_reaches_upstream_without_shared_key(pool: PgPool) {
        use crate::identity::{NewCredential, NewPrincipal};

        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = seen.clone();
        let upstream = Router::new().route("/v1/responses", post(move |headers: HeaderMap| {
            let captured = captured.clone();
            async move {
                let auth = headers.get("authorization").map(|value| value.to_str().unwrap().to_owned())
                    .unwrap_or_else(|| format!("api:{}", headers.get("x-api-key").unwrap().to_str().unwrap()));
                captured.lock().unwrap().push(auth);
                Json(json!({"id":"resp_test","object":"response","status":"completed","model":"upstream-model","output":[],"usage":{"input_tokens":1,"output_tokens":1}}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let tenant = Uuid::new_v4();
        sqlx::query(r#"WITH p AS (INSERT INTO providers (id, tenant_id, provider_key, display_name, provider_type, base_url, credential_ref, supported_operations, lifecycle_state) VALUES ($1,$2,'openai-test','OpenAI','openai',$3,'env://OPENAI_API_KEY',ARRAY['openai_responses'],'active') RETURNING id) INSERT INTO models (id, tenant_id, provider_id, public_model_id, provider_model_id, display_name, supported_operations, lifecycle_state) SELECT $4,$2,id,'personal-model','upstream-model','Personal',ARRAY['openai_responses'],'active' FROM p"#)
            .bind(Uuid::new_v4()).bind(tenant).bind(&base_url).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
        let root = std::env::temp_dir().join(format!("personal-api-flow-{}", Uuid::new_v4()));
        let mut config = test_config();
        config.openai_base_url = base_url;
        config.openai_api_key.clear();
        config.anthropic_api_key.clear();
        config.anthropic_personal_token_dir = Some(root);
        config.provider_allowed_tenant_ids = vec![tenant];
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let mut keys = Vec::new();
        for (name, personal_key) in [
            ("alice", Some("alice-api-key")),
            ("bob", Some("bob-api-key")),
            ("missing", None),
        ] {
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
            if let Some(secret) = personal_key {
                state
                    .personal_tokens
                    .as_ref()
                    .unwrap()
                    .api_store("openai-test")
                    .replace(tenant, ids.user_id, secret)
                    .await
                    .unwrap();
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
        let app = router(state);
        for (index, key) in keys.iter().enumerate() {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/responses")
                        .header("authorization", format!("Bearer {key}"))
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"model":"personal-model","input":"hello"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            if index < 2 {
                assert_eq!(response.status(), StatusCode::OK);
            } else {
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            }
        }
        assert_eq!(
            *seen.lock().unwrap(),
            ["Bearer alice-api-key", "Bearer bob-api-key"]
        );
        server.abort();
    }

    #[sqlx::test]
    async fn personal_tokens_follow_authenticated_gateway_requests(pool: PgPool) {
        use crate::identity::{NewCredential, NewPrincipal};

        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = seen.clone();
        let upstream = Router::new().route("/v1/messages", post(move |headers: HeaderMap| {
            let captured = captured.clone();
            async move {
                let auth = headers.get("authorization").map(|value| value.to_str().unwrap().to_owned())
                    .unwrap_or_else(|| format!("api:{}", headers.get("x-api-key").unwrap().to_str().unwrap()));
                captured.lock().unwrap().push(auth);
                Json(json!({"id":"msg_test","type":"message","role":"assistant","model":"upstream-model","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let tenant = Uuid::new_v4();
        sqlx::query(r#"WITH p AS (INSERT INTO providers (id, tenant_id, provider_key, display_name, provider_type, base_url, credential_ref, supported_operations, lifecycle_state) VALUES ($1,$2,'anthropic-test','Anthropic','anthropic',$3,'env://ANTHROPIC_SUBSCRIPTION_TOKEN_FILE',ARRAY['anthropic_messages'],'active') RETURNING id) INSERT INTO models (id, tenant_id, provider_id, public_model_id, provider_model_id, display_name, supported_operations, lifecycle_state) SELECT $4,$2,id,'personal-model','upstream-model','Personal',ARRAY['anthropic_messages'],'active' FROM p"#)
            .bind(Uuid::new_v4()).bind(tenant).bind(&base_url).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
        sqlx::query(r#"WITH p AS (INSERT INTO providers (id, tenant_id, provider_key, display_name, provider_type, base_url, credential_ref, supported_operations, lifecycle_state) VALUES ($1,$2,'anthropic-api','Anthropic API','anthropic',$3,'env://ANTHROPIC_PERSONAL_API',ARRAY['anthropic_messages'],'active') RETURNING id) INSERT INTO models (id, tenant_id, provider_id, public_model_id, provider_model_id, display_name, supported_operations, lifecycle_state) SELECT $4,$2,id,'api-model','upstream-model','API',ARRAY['anthropic_messages'],'active' FROM p"#)
            .bind(Uuid::new_v4()).bind(tenant).bind(&base_url).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
        let root = std::env::temp_dir().join(format!("personal-flow-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut config = test_config();
        config.anthropic_base_url = base_url;
        config.anthropic_subscription_token_file = Some(root.join("legacy"));
        config.anthropic_subscription_token_seed = Some("sk-ant-oat01-legacy".to_owned());
        config.anthropic_personal_token_dir = Some(root.join("users"));
        config.provider_allowed_tenant_ids = vec![tenant];
        config.extra_provider_profiles.push(ExtraProviderProfile {
            kind: ProviderKind::Anthropic,
            credential_ref: "env://ANTHROPIC_PERSONAL_API".to_owned(),
            provider_key: "anthropic-api".to_owned(),
            base_url: config.anthropic_base_url.clone(),
            operations: vec![GatewayOperation::AnthropicMessages],
            secret: None,
        });
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let mut keys = Vec::new();
        for (name, token) in [
            ("alice", Some("sk-ant-oat01-alice")),
            ("bob", Some("sk-ant-oat01-bob")),
            ("missing", None),
        ] {
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
            if let Some(token) = token {
                state
                    .personal_tokens
                    .as_ref()
                    .unwrap()
                    .replace(tenant, ids.user_id, token)
                    .await
                    .unwrap();
            }
            if name == "alice" {
                state
                    .personal_tokens
                    .as_ref()
                    .unwrap()
                    .api_store("anthropic-api")
                    .replace(tenant, ids.user_id, "alice-anthropic-api")
                    .await
                    .unwrap();
            }
            let key = state
                .credentials
                .create(NewCredential {
                    tenant_id: tenant,
                    display_name: name,
                    principal: ids,
                    expires_at: None,
                })
                .await
                .unwrap();
            keys.push(key.plaintext);
        }
        let app = router(state);
        let logs = logging::capture::Buffer::default();
        for (index, key) in keys.iter().enumerate() {
            let (subscriber, _provider) = logging::test_subscriber(logs.clone());
            let response = app.clone().oneshot(Request::builder().method("POST").uri("/v1/messages")
                .header("x-api-key", key).header("anthropic-version", "2023-06-01").header("content-type", "application/json")
                .body(Body::from(r#"{"model":"personal-model","max_tokens":8,"messages":[{"role":"user","content":"hello"}]}"#)).unwrap()).with_subscriber(subscriber).await.unwrap();
            if index < 2 {
                assert_eq!(response.status(), StatusCode::OK);
            } else {
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            }
        }
        let api_reply = app.clone().oneshot(Request::builder().method("POST").uri("/v1/messages")
            .header("x-api-key", &keys[0]).header("anthropic-version", "2023-06-01").header("content-type", "application/json")
            .body(Body::from(r#"{"model":"api-model","max_tokens":8,"messages":[{"role":"user","content":"hello"}]}"#)).unwrap()).await.unwrap();
        assert_eq!(api_reply.status(), StatusCode::OK);
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "Bearer sk-ant-oat01-alice",
                "Bearer sk-ant-oat01-bob",
                "api:alice-anthropic-api"
            ]
        );
        let lines = logs.lines();
        assert!(!lines.is_empty());
        let serialized = serde_json::to_string(&lines).unwrap();
        for secret in [
            "sk-ant-oat01-alice",
            "sk-ant-oat01-bob",
            "sk-ant-oat01-legacy",
        ] {
            assert!(!serialized.contains(secret));
        }
        for key in &keys {
            assert!(!serialized.contains(key));
        }
        let injected = format!("{serialized} sk-ant-oat01-alice");
        assert!(injected.contains("sk-ant-oat01-alice"));
        server.abort();
    }

    #[sqlx::test]
    async fn personal_gemini_and_xai_keys_follow_the_authenticated_user(pool: PgPool) {
        use crate::identity::{NewCredential, NewPrincipal};
        let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let gemini_seen = Arc::clone(&seen);
        let gemini_stream_seen = Arc::clone(&seen);
        let xai_seen = Arc::clone(&seen);
        let upstream = Router::new()
            .route("/v1beta/models/gemini-target:generateContent", post(move |headers: HeaderMap| {
                let seen = Arc::clone(&gemini_seen);
                async move {
                    seen.lock().unwrap().push(("gemini".to_owned(), headers["x-goog-api-key"].to_str().unwrap().to_owned()));
                    Json(json!({"candidates":[{"content":{"role":"model","parts":[{"text":"hello"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3}}))
                }
            }))
            .route("/v1beta/models/gemini-target:streamGenerateContent", post(move |headers: HeaderMap| {
                let seen = Arc::clone(&gemini_stream_seen);
                async move {
                    seen.lock().unwrap().push(("gemini-stream".to_owned(), headers["x-goog-api-key"].to_str().unwrap().to_owned()));
                    ([("content-type", "text/event-stream")], concat!(
                        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello\"}]}}]}\n\n",
                        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\" world\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":1}}\n\n"
                    ))
                }
            }))
            .route("/v1/responses", post(move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
                let seen = Arc::clone(&xai_seen);
                async move {
                    seen.lock().unwrap().push(("xai".to_owned(), headers["authorization"].to_str().unwrap().to_owned()));
                    if body["stream"] == true {
                        ([("content-type", "text/event-stream")], concat!(
                            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_xai\"},\"sequence_number\":0}\n\n",
                            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\",\"sequence_number\":1}\n\n",
                            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_xai\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}},\"sequence_number\":2}\n\n"
                        )).into_response()
                    } else {
                        Json(json!({"id":"resp_xai","object":"response","status":"completed","model":"xai-target","output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]}],"usage":{"input_tokens":1,"output_tokens":1}})).into_response()
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base_url = format!("http://{address}");
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let tenant = Uuid::new_v4();
        for (provider_key, provider_type, credential_ref, operation, model, target) in [
            (
                "personal-gemini",
                "gemini",
                "env://GEMINI_TEST",
                "gemini_generate_content",
                "gemini-public",
                "gemini-target",
            ),
            (
                "personal-xai",
                "xai",
                "env://XAI_TEST",
                "openai_responses",
                "xai-public",
                "xai-target",
            ),
        ] {
            sqlx::query(r#"WITH p AS (INSERT INTO providers (id, tenant_id, provider_key, display_name, provider_type, base_url, credential_ref, supported_operations, lifecycle_state) VALUES ($1,$2,$3,$3,$4,$5,$6,ARRAY[$7]::text[],'active') RETURNING id) INSERT INTO models (id,tenant_id,provider_id,public_model_id,provider_model_id,display_name,supported_operations,supports_streaming,lifecycle_state) SELECT $8,$2,id,$9,$10,$9,ARRAY[$7]::text[],true,'active' FROM p"#)
                .bind(Uuid::new_v4()).bind(tenant).bind(provider_key).bind(provider_type).bind(&base_url).bind(credential_ref).bind(operation).bind(Uuid::new_v4()).bind(model).bind(target).execute(&pool).await.unwrap();
        }
        let mut config = test_config();
        config.provider_allowed_tenant_ids = vec![tenant];
        config.anthropic_personal_token_dir =
            Some(std::env::temp_dir().join(format!("personal-providers-{}", Uuid::new_v4())));
        config.extra_provider_profiles.extend([
            ExtraProviderProfile {
                kind: ProviderKind::Gemini,
                credential_ref: "env://GEMINI_TEST".to_owned(),
                provider_key: "personal-gemini".to_owned(),
                base_url: base_url.clone(),
                operations: vec![GatewayOperation::GeminiGenerateContent],
                secret: None,
            },
            ExtraProviderProfile {
                kind: ProviderKind::Xai,
                credential_ref: "env://XAI_TEST".to_owned(),
                provider_key: "personal-xai".to_owned(),
                base_url,
                operations: vec![GatewayOperation::OpenAiResponses],
                secret: None,
            },
        ]);
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let mut keys = Vec::new();
        for name in ["alice", "bob"] {
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
            let (provider, secret) = if name == "alice" {
                ("personal-gemini", "alice-gemini-secret")
            } else {
                ("personal-xai", "bob-xai-secret")
            };
            state
                .personal_tokens
                .as_ref()
                .unwrap()
                .api_store(provider)
                .replace(tenant, ids.user_id, secret)
                .await
                .unwrap();
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
        let app = router(state);
        for (index, model, expected) in [
            (0, "gemini-public", StatusCode::OK),
            (1, "gemini-public", StatusCode::SERVICE_UNAVAILABLE),
            (0, "xai-public", StatusCode::SERVICE_UNAVAILABLE),
            (1, "xai-public", StatusCode::OK),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/responses")
                        .header("authorization", format!("Bearer {}", keys[index]))
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"model":model,"input":"hello"}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            let bytes = to_bytes(response.into_body(), 16384).await.unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains("alice-gemini-secret") && !text.contains("bob-xai-secret"));
            if expected == StatusCode::OK {
                assert!(text.contains("hello"));
            }
        }
        for (index, model) in [(0, "gemini-public"), (1, "xai-public")] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/responses")
                        .header("authorization", format!("Bearer {}", keys[index]))
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"model":model,"input":"hello","stream":true}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), 16384).await.unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(text.contains("hello") && text.contains("response.completed"));
            assert!(!text.contains("alice-gemini-secret") && !text.contains("bob-xai-secret"));
        }
        for (index, model) in [(1, "gemini-public"), (0, "xai-public")] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/responses")
                        .header("authorization", format!("Bearer {}", keys[index]))
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"model":model,"input":"hello","stream":true}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                ("gemini".to_owned(), "alice-gemini-secret".to_owned()),
                ("xai".to_owned(), "Bearer bob-xai-secret".to_owned()),
                ("gemini-stream".to_owned(), "alice-gemini-secret".to_owned()),
                ("xai".to_owned(), "Bearer bob-xai-secret".to_owned())
            ]
        );
        server.abort();
    }

    #[tokio::test]
    async fn personal_token_revocation_waits_for_delayed_upstream_headers() {
        use crate::model_registry::ProviderProfile;

        let root = std::env::temp_dir().join(format!("personal-delay-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut config = test_config();
        config.anthropic_subscription_token_file = Some(root.join("legacy"));
        config.anthropic_personal_token_dir = Some(root.join("users"));
        let pool = db::pool(&config.database_url, 1).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let store = state.personal_tokens.as_ref().unwrap();
        store
            .replace(tenant, user, "sk-ant-oat01-user")
            .await
            .unwrap();
        let mut identity = principal(tenant);
        identity.user_id = Some(user);
        let profile = ProviderProfile::held_bearer(
            "env://ANTHROPIC_SUBSCRIPTION_TOKEN_FILE",
            "anthropic-test",
            "http://127.0.0.1:1",
            vec![GatewayOperation::AnthropicMessages],
            vec![tenant],
            state.held_token.clone().unwrap(),
        );
        let personal = state
            .profile_for_user(&(&identity).into(), &profile)
            .await
            .unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let server_started = started.clone();
        let server_release = release.clone();
        let upstream = Router::new().route(
            "/v1/messages",
            post(move || {
                let started = server_started.clone();
                let release = server_release.clone();
                async move {
                    started.notify_one();
                    release.notified().await;
                    StatusCode::OK
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let adapter = crate::adapters::provider::for_kind(ProviderKind::Anthropic).unwrap();
        let headers = axum::http::HeaderMap::new();
        let input = crate::adapters::ProviderRequestInput {
            source_path_and_query: "/v1/messages",
            client_headers: &headers,
            idempotency_key: None,
        };
        let plan = adapter
            .plan_for_target(
                GatewayOperation::AnthropicMessages,
                GatewayOperation::AnthropicMessages,
                "provider-model",
                false,
                &input,
            )
            .unwrap();
        let prepared = adapter
            .send(
                plan,
                axum::body::Bytes::from_static(b"{}"),
                &personal,
                input,
            )
            .unwrap();
        let client = state.client.clone();
        let sender = tokio::spawn(async move {
            let response = crate::adapters::provider::send(
                &client,
                &format!("http://{address}"),
                prepared,
                "anthropic-test",
                "provider-model",
            )
            .await;
            drop(personal);
            response
        });
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        assert!(store.revoke(tenant, user).await.is_err());
        release.notify_one();
        assert_eq!(sender.await.unwrap().unwrap().status(), StatusCode::OK);
        store.revoke(tenant, user).await.unwrap();
        assert!(store.get(tenant, user).await.unwrap().is_none());
        server.abort();
    }

    #[tokio::test]
    async fn codex_profile_requires_an_explicit_owner_with_or_without_personal_tokens() {
        use crate::model_registry::ProviderProfile;

        let root = std::env::temp_dir().join(format!("codex-owner-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let tenant = Uuid::new_v4();
        let owner = Uuid::new_v4();
        let mut identity = principal(tenant);
        identity.user_id = Some(owner);
        let profile =
            ProviderProfile::codex_file(root.join("auth.json"), "owner".to_owned(), vec![tenant]);
        for personal in [false, true] {
            for configured_owner in [None, Some(Uuid::new_v4()), Some(owner)] {
                let mut config = test_config();
                config.codex_owner_user_id = configured_owner;
                config.anthropic_personal_token_dir = personal.then(|| root.join("personal"));
                let pool = db::pool(&config.database_url, 1).unwrap();
                let spool = config.ledger_spool_dir.clone();
                let state = AppState::new(config, pool).unwrap();
                let selected = state.profile_for_user(&(&identity).into(), &profile).await;
                assert_eq!(selected.is_ok(), configured_owner == Some(owner));
                if let Ok(selected) = selected {
                    assert!(selected.is_codex_subscription());
                    assert!(selected.codex_credentials().is_err());
                }
                identity.user_id = None;
                assert!(
                    state
                        .profile_for_user(&(&identity).into(), &profile)
                        .await
                        .is_err()
                );
                identity.user_id = Some(owner);
                drop(state);
                std::fs::remove_dir_all(spool).unwrap();
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn personal_profile_never_falls_back_to_another_users_token() {
        use crate::model_registry::ProviderProfile;

        let root = std::env::temp_dir().join(format!("personal-profile-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut config = test_config();
        config.anthropic_subscription_token_file = Some(root.join("legacy"));
        config.anthropic_subscription_token_seed = Some("sk-ant-oat01-legacy".to_owned());
        config.anthropic_personal_token_dir = Some(root.join("users"));
        let pool = db::pool(&config.database_url, 1).unwrap();
        let state = AppState::new(config, pool).unwrap();
        let tenant = Uuid::new_v4();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let store = state.personal_tokens.as_ref().unwrap();
        store
            .replace(tenant, alice, "sk-ant-oat01-alice")
            .await
            .unwrap();
        store
            .replace(tenant, bob, "sk-ant-oat01-bob")
            .await
            .unwrap();
        let profile = ProviderProfile::held_bearer(
            "env://ANTHROPIC_SUBSCRIPTION_TOKEN_FILE",
            "anthropic-test",
            "http://127.0.0.1:1",
            vec![GatewayOperation::AnthropicMessages],
            vec![tenant],
            state.held_token.clone().unwrap(),
        );
        let mut identity = principal(tenant);
        identity.user_id = Some(alice);
        let shared = ProviderProfile::bearer(
            "env://OPENAI_API_KEY",
            "openai-test",
            "http://127.0.0.1:1",
            crate::model_registry::ProviderDialect::OpenAi,
            vec![GatewayOperation::OpenAiResponses],
            vec![tenant],
            Arc::from("shared-secret"),
        );
        assert!(
            state
                .profile_for_user(&(&identity).into(), &shared)
                .await
                .is_err()
        );
        store
            .api_store("openai-test")
            .replace(tenant, alice, "alice-openai-key")
            .await
            .unwrap();
        let personal_api = state
            .profile_for_user(&(&identity).into(), &shared)
            .await
            .unwrap();
        assert_eq!(personal_api.bearer_secret(), Some("alice-openai-key"));
        drop(personal_api);
        let local = ProviderProfile::unauthenticated_for(
            ProviderKind::Ollama,
            "local://model",
            "local",
            "http://127.0.0.1:1",
            vec![GatewayOperation::OpenAiResponses],
            vec![tenant],
        );
        assert!(
            state
                .profile_for_user(&(&identity).into(), &local)
                .await
                .is_ok()
        );
        let alice_profile = state
            .profile_for_user(&(&identity).into(), &profile)
            .await
            .unwrap();
        assert_eq!(&*alice_profile.held_token().unwrap(), "sk-ant-oat01-alice");
        identity.user_id = Some(bob);
        let bob_profile = state
            .profile_for_user(&(&identity).into(), &profile)
            .await
            .unwrap();
        assert_eq!(&*bob_profile.held_token().unwrap(), "sk-ant-oat01-bob");
        let received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let captured = received.clone();
        let upstream = Router::new().route(
            "/v1/messages",
            post(move |headers: axum::http::HeaderMap| {
                let captured = captured.clone();
                async move {
                    captured.lock().await.push(
                        headers
                            .get("authorization")
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned(),
                    );
                    StatusCode::OK
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let adapter = crate::adapters::provider::for_kind(ProviderKind::Anthropic).unwrap();
        let client_headers = axum::http::HeaderMap::new();
        for chosen in [&alice_profile, &bob_profile] {
            let input = crate::adapters::ProviderRequestInput {
                source_path_and_query: "/v1/messages",
                client_headers: &client_headers,
                idempotency_key: None,
            };
            let plan = adapter
                .plan_for_target(
                    GatewayOperation::AnthropicMessages,
                    GatewayOperation::AnthropicMessages,
                    "provider-model",
                    false,
                    &input,
                )
                .unwrap();
            let prepared = adapter
                .send(plan, axum::body::Bytes::from_static(b"{}"), chosen, input)
                .unwrap();
            let response = crate::adapters::provider::send(
                &state.client,
                &format!("http://{address}"),
                prepared,
                "anthropic-test",
                "provider-model",
            )
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert_eq!(
            *received.lock().await,
            ["Bearer sk-ant-oat01-alice", "Bearer sk-ant-oat01-bob"]
        );
        server.abort();
        assert!(store.revoke(tenant, alice).await.is_err());
        drop(alice_profile);
        drop(bob_profile);
        identity.user_id = None;
        assert!(
            state
                .profile_for_user(&(&identity).into(), &profile)
                .await
                .is_err()
        );
        identity.user_id = Some(alice);
        identity.tenant_id = Uuid::new_v4();
        assert!(
            state
                .profile_for_user(&(&identity).into(), &profile)
                .await
                .is_err()
        );
        identity.tenant_id = tenant;
        store.revoke(tenant, alice).await.unwrap();
        assert!(
            state
                .profile_for_user(&(&identity).into(), &profile)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn admin_spa_has_strict_csp_and_does_not_shadow_the_api() {
        let app = router(test_state());
        let page = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin/requests")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        assert_eq!(
            page.headers()
                .get("content-security-policy")
                .unwrap()
                .to_str()
                .unwrap(),
            "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; font-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
        );

        let api = app
            .oneshot(
                Request::builder()
                    .uri("/admin/v1/overview")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(api.status(), StatusCode::UNAUTHORIZED);
    }

    /// The one failure this guards against is silent: if the access log stops
    /// seeing the span, `trace_id` becomes empty, the formatter drops empty
    /// fields, and the line still looks perfectly well formed.
    #[tokio::test]
    async fn every_request_is_logged_with_a_trace_id() {
        let buffer = logging::capture::Buffer::default();
        let (subscriber, _provider) = logging::test_subscriber(buffer.clone());

        let state = test_state();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .with_subscriber(subscriber)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let lines = buffer.lines();
        let access = lines
            .iter()
            .find(|line| line["event"]["name"] == "http.access")
            .unwrap_or_else(|| panic!("no access log line in {lines:?}"));

        let trace_id = access["trace_id"].as_str().unwrap_or_default();
        assert_eq!(
            trace_id.len(),
            32,
            "no trace id on the access line: {access}"
        );
        assert_eq!(access["span_id"].as_str().unwrap_or_default().len(), 16);
        assert_eq!(access["http"]["route"], "/healthz");
        assert_eq!(access["http"]["response"]["status_code"], 200);
        assert!(
            !access["request_id"].as_str().unwrap_or_default().is_empty(),
            "no request id: {access}"
        );
    }

    /// Drives a request through the real router and returns its access line.
    async fn access_line(request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let buffer = logging::capture::Buffer::default();
        let (subscriber, _provider) = logging::test_subscriber(buffer.clone());

        let state = test_state();
        let response = router(state)
            .oneshot(request)
            .with_subscriber(subscriber)
            .await
            .unwrap();
        let status = response.status();

        let lines = buffer.lines();
        let access = lines
            .iter()
            .find(|line| line["event"]["name"] == "http.access")
            .unwrap_or_else(|| panic!("no access log line in {lines:?}"))
            .clone();
        (status, access)
    }

    /// Liveness and readiness must not be the same answer wearing two names.
    ///
    /// The test database points at a port nobody listens on, so this is the
    /// negative control for the split: the dependency is down, and only the
    /// endpoint that is supposed to notice does.
    #[tokio::test]
    async fn readiness_fails_while_liveness_holds_when_the_database_is_down() {
        for path in ["/health", "/healthz"] {
            let response = router(test_state())
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{path} must not depend on the database"
            );
        }

        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "/ready reported ready with an unreachable database"
        );
    }

    #[tokio::test]
    async fn health_counts_active_requests_until_the_last_handle_is_released() {
        let state = test_state();
        let app = router(Arc::clone(&state));
        let active = state.lifecycle.admit().unwrap();
        let mut retained = Some(active.clone());
        drop(active);

        for expected in [1, 0] {
            for path in ["/health", "/healthz"] {
                let response = app
                    .clone()
                    .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let body: serde_json::Value =
                    serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                        .unwrap();
                assert_eq!(body["active_requests"], expected);
                assert_eq!(state.lifecycle.active(), expected as usize);
            }
            drop(retained.take());
        }
    }

    #[tokio::test]
    async fn endpoint_body_limits_are_independent_and_return_stable_413() {
        let upstream = Router::new().route(
            "/v1/messages",
            post(|| async {
                Json(json!({
                    "id": "anthropic-with-larger-body",
                    "type": "message",
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_request_max_bytes = 64;
        config.anthropic_request_max_bytes = 512;
        config.anthropic_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());

        let openai = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "company-default", "input": "x".repeat(100)}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(openai.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(openai.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "payload_too_large");

        let anthropic = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", "client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "company-default", "padding": "x".repeat(100)}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(anthropic.status(), StatusCode::OK);

        state.ledger.wait_for_idle().await;
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    /// A caller's trace must continue through the gateway. Without this, one
    /// request crossing two services looks like two unrelated requests, and
    /// the id the caller quotes in a ticket finds nothing here.
    #[tokio::test]
    async fn an_incoming_trace_is_continued() {
        let incoming = "4bf92f3577b34da6a3ce929d0e0e4736";
        let (status, access) = access_line(
            Request::builder()
                .uri("/healthz")
                .header("traceparent", format!("00-{incoming}-00f067aa0ba902b7-01"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            access["trace_id"], incoming,
            "the caller's trace was not continued: {access}"
        );
    }

    /// A caller whose tracing is broken still gets served, and still gets a
    /// trace of their own -- refusing the request, or leaving it untraced,
    /// would both be worse than ignoring the bad header.
    #[tokio::test]
    async fn a_malformed_traceparent_is_ignored() {
        let (status, access) = access_line(
            Request::builder()
                .uri("/healthz")
                .header("traceparent", "not-a-trace-parent")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            access["trace_id"].as_str().unwrap_or_default().len(),
            32,
            "no trace of its own: {access}"
        );
    }

    /// A rejected key must be visible as a warning with a stable identifier.
    /// It carries no cause: the type says everything, and the request that
    /// caused it belongs to the caller.
    #[tokio::test]
    async fn a_rejected_key_is_logged_as_a_warning() {
        let (status, access) = access_line(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"company-default"}"#))
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(access["severity_number"], 13, "not a warning: {access}");
        assert_eq!(access["error"]["type"], "unauthorized");
        assert_eq!(access["http"]["response"]["status_code"], 401);
        assert!(
            access["exception"]["message"].is_null(),
            "a rejected key needs no cause: {access}"
        );
    }

    /// An unreachable upstream must be an error *with* the cause: without it
    /// the line says only that a 502 happened, never whether it was DNS, TLS
    /// or a timeout. The test config points at a port nobody listens on.
    #[tokio::test]
    async fn an_unreachable_upstream_is_logged_with_its_cause() {
        let (status, access) = access_line(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer client-key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"company-default"}"#))
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(access["severity_number"], 17, "not an error: {access}");
        assert_eq!(access["error"]["type"], "upstream_unavailable");
        assert!(
            !access["exception"]["message"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "no cause on a 502: {access}"
        );
    }

    #[tokio::test]
    async fn provider_idle_timeout_calls_the_upstream_once_and_returns_502() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_by_server = Arc::clone(&calls);
        let upstream = Router::new().route(
            "/v1/responses",
            post(move || {
                let calls = Arc::clone(&calls_by_server);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Json(json!({"id": "too-late", "status": "completed", "output": []}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        config.provider_idle_timeout = Duration::from_millis(20);
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        state.ledger.wait_for_idle().await;
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn local_provider_timeout_is_longer_without_changing_cloud_timeout() {
        let upstream = Router::new().route(
            "/",
            axum::routing::get(|| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                StatusCode::OK
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.provider_idle_timeout = Duration::from_millis(25);
        config.local_provider_idle_timeout = Duration::from_millis(500);
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = AppState::new(config, pool).unwrap();
        let url = format!("http://{address}/");
        assert!(
            state
                .client_for_provider(ProviderKind::OpenAi)
                .get(&url)
                .send()
                .await
                .is_err()
        );
        assert_eq!(
            state
                .client_for_provider(ProviderKind::Ollama)
                .get(&url)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        server.abort();
    }

    #[sqlx::test]
    async fn local_provider_request_uses_its_own_timeout(pool: PgPool) {
        let upstream = Router::new().route("/v1/responses", post(|| async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Json(json!({"id":"resp_local","object":"response","status":"completed","model":"local-target","output":[]}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let tenant_id = Uuid::new_v4();
        let base_url = format!("http://{address}");
        sqlx::query(r#"WITH p AS (INSERT INTO providers (id, tenant_id, provider_key, display_name, provider_type, base_url, credential_ref, supported_operations, lifecycle_state) VALUES ($1,$2,'local-test','Local','local',$3,'env://LOCAL_TEST',ARRAY['openai_responses'],'active') RETURNING id) INSERT INTO models (id, tenant_id, provider_id, public_model_id, provider_model_id, display_name, supported_operations, lifecycle_state) SELECT $4,$2,id,'local-model','local-target','Local',ARRAY['openai_responses'],'active' FROM p"#)
            .bind(Uuid::new_v4()).bind(tenant_id).bind(&base_url).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
        let mut config = test_config();
        config.provider_allowed_tenant_ids = vec![tenant_id];
        config.provider_idle_timeout = Duration::from_millis(25);
        config.local_provider_idle_timeout = Duration::from_millis(500);
        config.extra_provider_profiles.push(ExtraProviderProfile {
            kind: ProviderKind::Ollama,
            credential_ref: "env://LOCAL_TEST".to_owned(),
            provider_key: "local-test".to_owned(),
            base_url,
            operations: vec![GatewayOperation::OpenAiResponses],
            secret: None,
        });
        let state = AppState::new(config, pool).unwrap();
        let result = openai::execute(
            &state,
            RequestContext {
                principal: principal(tenant_id),
                received_at: Utc::now(),
                path_and_query: "/v1/responses".to_owned(),
                headers: HeaderMap::new(),
                body: Bytes::from_static(br#"{"model":"local-model","input":"hello"}"#),
                operation: OpenAiOperation::Responses,
            },
        )
        .await
        .unwrap();
        assert_eq!(result.status, StatusCode::OK);
        server.abort();
    }

    #[sqlx::test]
    async fn a_successful_request_persists_request_attempt_and_usage(pool: PgPool) {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|headers: HeaderMap| async move {
                assert!(!headers.contains_key("x-apg-task-id"));
                (
                    [("x-request-id", "provider-request")],
                    Json(json!({
                        "id": "provider-response",
                        "choices": [],
                        "usage": {
                            "prompt_tokens": 10,
                            "completion_tokens": 3,
                            "prompt_tokens_details": {"cached_tokens": 4},
                            "cache_creation_input_tokens": 2,
                            "output_tokens_details": {"reasoning_tokens": 1}
                        }
                    })),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });

        let mut config = test_config();
        config.content_capture_enabled = false;
        config.openai_base_url = format!("http://{address}");
        config.openai_provider = "test-provider".to_owned();
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .header("x-session-id", "session-one")
                    .header("x-apg-task-id", "successful-task")
                    .header("x-apg-prompt-id", "x".repeat(129))
                    .body(Body::from(
                        r#"{"model":"company-default","messages":[{"role":"user","content":"sentinel-prompt"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        state.ledger.flush_startup().await.unwrap();

        let row = sqlx::query(
            r#"
            SELECT gr.request_id, gr.session_id, gr.requested_model,
                   gr.selected_provider, pa.provider_request_id,
                   pa.provider_response_id, ur.input_tokens, ur.output_tokens,
                   ur.cached_input_tokens, ur.cache_write_input_tokens,
                   ur.reasoning_tokens, gr.policy_decision,
                   (SELECT task_id FROM request_attribution a
                    WHERE (a.tenant_id, a.gateway_request_id) = (gr.tenant_id, gr.id)) AS task_id,
                   (SELECT prompt_id FROM request_attribution a
                    WHERE (a.tenant_id, a.gateway_request_id) = (gr.tenant_id, gr.id)) AS prompt_id
            FROM gateway_requests gr
            JOIN provider_attempts pa
              ON (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id)
            JOIN usage_records ur
              ON (ur.tenant_id, ur.gateway_request_id, ur.provider_attempt_id) =
                 (pa.tenant_id, pa.gateway_request_id, pa.id)
            WHERE ur.source = 'provider_reported'
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!row.get::<String, _>("request_id").is_empty());
        assert_eq!(row.get::<String, _>("session_id"), "session-one");
        assert_eq!(row.get::<String, _>("task_id"), "successful-task");
        assert_eq!(row.get::<Option<String>, _>("prompt_id"), None);
        assert_eq!(row.get::<String, _>("requested_model"), "company-default");
        assert_eq!(row.get::<String, _>("selected_provider"), "test-provider");
        assert_eq!(
            row.get::<String, _>("provider_request_id"),
            "provider-request"
        );
        assert_eq!(
            row.get::<String, _>("provider_response_id"),
            "provider-response"
        );
        assert_eq!(row.get::<i64, _>("input_tokens"), 4);
        assert_eq!(row.get::<i64, _>("output_tokens"), 2);
        assert_eq!(row.get::<i64, _>("cached_input_tokens"), 4);
        assert_eq!(row.get::<i64, _>("cache_write_input_tokens"), 2);
        assert_eq!(row.get::<i64, _>("reasoning_tokens"), 1);
        let policy: serde_json::Value = row.get("policy_decision");
        assert_eq!(policy["routing"]["attempt_kind"], "initial");
        assert_eq!(policy["routing"]["reason"], "registry");
        assert_eq!(policy["routing"]["same_request_retry"], false);
        assert_eq!(policy["effective"]["content_capture"], "metadata");
        upstream_task.abort();
        std::fs::remove_dir_all(&state.config.ledger_spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn prepared_requests_are_encrypted_before_upstream_send(pool: PgPool) {
        use std::os::unix::fs::PermissionsExt;
        let base = std::path::Path::new("/tmp").join(format!("capture-request-{}", Uuid::new_v4()));
        let spool = base.join("spool");
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        for path in [&base, &spool, &keys, &anchors] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        crate::capture_keyring::rotate(&keys, &anchors, &spool).unwrap();
        let mut config = test_config();
        config.content_capture_enabled = true;
        config.content_capture_spool_dir = spool.clone();
        config.content_capture_key_dir = keys.clone();
        config.content_capture_anchor_dir = anchors;
        let state = AppState::new(config, pool).unwrap();
        let principal = PrincipalContext {
            tenant_id: Uuid::new_v4(),
            credential_id: Uuid::new_v4(),
            credential_name: "fixture".to_owned(),
            user_id: Some(Uuid::new_v4()),
            user_name: None,
            device_id: None,
            device_name: None,
            client_id: None,
            client_name: None,
            agent_id: None,
            agent_name: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer gateway-secret".parse().unwrap());
        let selector = crate::model_registry::ModelSelector::parse("company-default").unwrap();
        let policy = state
            .policies
            .evaluate(&principal, &headers, &selector, Utc::now())
            .await
            .unwrap();
        let original = Bytes::from_static(br#"{"model":"company-default","messages":[{"role":"user","content":"private-prompt"}]}"#);
        let decoded = crate::adapters::protocol::decode(
            GatewayOperation::OpenAiChatCompletions,
            original.clone(),
        )
        .unwrap();
        let id = Uuid::new_v4();
        let mut canonical = decoded.enrich(crate::canonical::RequestEnrichment {
            gateway_request_id: id,
            trace_id: None,
            session_id: None,
            principal: (&principal).into(),
            requested_model: "company-default".to_owned(),
            requested_quality: None,
            workload_class: "interactive".to_owned(),
            urgency: "normal".to_owned(),
            budget_class: "normal".to_owned(),
            deadline: None,
        });
        let upstream = br#"{"model":"provider-model","messages":[{"role":"user","content":"private-prompt"}]}"#;
        let session = crate::services::capture_request::start(
            &state,
            &policy,
            &canonical,
            &headers,
            upstream,
            Some("provider-secret"),
        )
        .await
        .unwrap()
        .unwrap();
        drop(session);
        let path = spool.join(format!("{id}.capture"));
        let encrypted = std::fs::read(&path).unwrap();
        assert!(
            !encrypted
                .windows(b"private-prompt".len())
                .any(|window| window == b"private-prompt")
        );
        let recovered =
            crate::capture_spool::CaptureSpool::recover_with_keyring(&path, &keys, 4096).unwrap();
        assert!(!recovered.complete);
        let first = recovered
            .cipher
            .unseal_timed(
                recovered.blocks[0].0,
                recovered.blocks[0].1,
                0,
                recovered.elapsed_ns[0],
                &recovered.blocks[0].2,
            )
            .unwrap();
        let second = recovered
            .cipher
            .unseal_timed(
                recovered.blocks[1].0,
                recovered.blocks[1].1,
                1,
                recovered.elapsed_ns[1],
                &recovered.blocks[1].2,
            )
            .unwrap();
        assert_eq!(first, original);
        assert_eq!(second, upstream);
        let mut metadata = policy.clone();
        metadata.disable_content_capture();
        assert!(
            crate::services::capture_request::start(
                &state,
                &metadata,
                &canonical,
                &headers,
                upstream,
                Some("provider-secret")
            )
            .await
            .unwrap()
            .is_none()
        );
        assert_eq!(std::fs::read(&path).unwrap(), encrypted);
        canonical.principal.user_id = None;
        assert!(
            crate::services::capture_request::start(
                &state,
                &policy,
                &canonical,
                &headers,
                upstream,
                Some("provider-secret")
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), encrypted);
        canonical.principal.user_id = principal.user_id;
        canonical.gateway_request_id = Uuid::new_v4();
        let session = crate::services::capture_request::start(
            &state,
            &policy,
            &canonical,
            &headers,
            upstream,
            Some("provider-secret"),
        )
        .await
        .unwrap();
        crate::services::capture_request::finish_buffered(
            session,
            b"provider-response",
            b"client-response",
        )
        .await
        .unwrap();
        let completed = crate::capture_spool::CaptureSpool::recover_with_keyring(
            &spool.join(format!("{}.capture", canonical.gateway_request_id)),
            &keys,
            4096,
        )
        .unwrap();
        assert!(completed.complete);
        assert_eq!(completed.blocks.len(), 5);
        canonical.gateway_request_id = Uuid::new_v4();
        let interrupted = crate::services::capture_request::start(
            &state,
            &policy,
            &canonical,
            &headers,
            upstream,
            Some("provider-secret"),
        )
        .await
        .unwrap();
        crate::services::capture_request::save_buffered(
            &interrupted,
            b"provider-response",
            b"client-response",
        )
        .await
        .unwrap();
        drop(interrupted);
        let pending = crate::capture_spool::CaptureSpool::recover_with_keyring(
            &spool.join(format!("{}.capture", canonical.gateway_request_id)),
            &keys,
            4096,
        )
        .unwrap();
        assert!(!pending.complete);
        assert_eq!(pending.blocks.len(), 4);
        for (position, expected) in [
            original.as_ref(),
            upstream.as_slice(),
            b"provider-response",
            b"client-response",
        ]
        .into_iter()
        .enumerate()
        {
            let (leg, sequence, sealed) = &completed.blocks[position];
            let plaintext = completed
                .cipher
                .unseal_timed(
                    *leg,
                    *sequence,
                    position as u64,
                    completed.elapsed_ns[position],
                    sealed,
                )
                .unwrap();
            assert_eq!(plaintext, expected);
        }
        canonical.gateway_request_id = Uuid::new_v4();
        let session = crate::services::capture_request::start(
            &state,
            &policy,
            &canonical,
            &headers,
            upstream,
            Some("provider-secret"),
        )
        .await
        .unwrap();
        let error = crate::error::GatewayError::UpstreamProtocol;
        crate::services::capture_request::finish_error(session, b"bad-provider-response", &error)
            .await
            .unwrap();
        let recorded = crate::capture_spool::CaptureSpool::recover_with_keyring(
            &spool.join(format!("{}.capture", canonical.gateway_request_id)),
            &keys,
            4096,
        )
        .unwrap();
        assert!(recorded.complete);
        let (leg, sequence, sealed) = &recorded.blocks[3];
        let client_error = recorded
            .cipher
            .unseal_timed(*leg, *sequence, 3, recorded.elapsed_ns[3], sealed)
            .unwrap();
        let actual = to_bytes(error.into_response().into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(client_error, actual);
        canonical.gateway_request_id = Uuid::new_v4();
        let pending_error = crate::services::capture_request::start(
            &state,
            &policy,
            &canonical,
            &headers,
            upstream,
            Some("provider-secret"),
        )
        .await
        .unwrap();
        crate::services::capture_request::save_error(
            &pending_error,
            b"bad-provider-response",
            &crate::error::GatewayError::UpstreamProtocol,
        )
        .await
        .unwrap();
        drop(pending_error);
        let pending = crate::capture_spool::CaptureSpool::recover_with_keyring(
            &spool.join(format!("{}.capture", canonical.gateway_request_id)),
            &keys,
            4096,
        )
        .unwrap();
        assert!(!pending.complete);
        assert_eq!(pending.blocks.len(), 4);
        canonical.gateway_request_id = Uuid::new_v4();
        let session = crate::services::capture_request::start(
            &state,
            &policy,
            &canonical,
            &headers,
            upstream,
            Some("provider-secret"),
        )
        .await
        .unwrap();
        crate::services::capture_request::finish_incomplete_error(
            session,
            b"partial-provider-response",
            &crate::error::GatewayError::UpstreamProtocol,
        )
        .await
        .unwrap();
        let interrupted = crate::capture_spool::CaptureSpool::recover_with_keyring(
            &spool.join(format!("{}.capture", canonical.gateway_request_id)),
            &keys,
            4096,
        )
        .unwrap();
        assert!(!interrupted.complete);
        assert_eq!(interrupted.blocks.len(), 4);
        let (leg, sequence, sealed) = &interrupted.blocks[2];
        assert_eq!(
            interrupted
                .cipher
                .unseal_timed(*leg, *sequence, 2, interrupted.elapsed_ns[2], sealed)
                .unwrap(),
            b"partial-provider-response"
        );
        canonical.gateway_request_id = Uuid::new_v4();
        let session = crate::services::capture_request::start(
            &state,
            &policy,
            &canonical,
            &headers,
            upstream,
            Some("provider-secret"),
        )
        .await
        .unwrap();
        assert!(
            crate::services::capture_request::finish_buffered(
                session,
                b"provider-secret",
                b"client-response"
            )
            .await
            .is_err()
        );
        let failed = crate::capture_spool::CaptureSpool::recover_with_keyring(
            &spool.join(format!("{}.capture", canonical.gateway_request_id)),
            &keys,
            4096,
        )
        .unwrap();
        assert!(!failed.complete);
        assert_eq!(failed.blocks.len(), 2);
        canonical.gateway_request_id = Uuid::new_v4();
        assert!(
            crate::services::capture_request::start(
                &state,
                &policy,
                &canonical,
                &headers,
                b"provider-secret",
                Some("provider-secret")
            )
            .await
            .is_err()
        );
        let partial =
            std::fs::read(spool.join(format!("{}.capture", canonical.gateway_request_id))).unwrap();
        assert!(
            !partial
                .windows(b"provider-secret".len())
                .any(|window| window == b"provider-secret")
        );
        std::fs::remove_dir_all(&state.config.ledger_spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn streaming_relay_saves_both_wire_directions_before_completion(pool: PgPool) {
        use std::os::unix::fs::PermissionsExt;
        let base = std::path::Path::new("/tmp").join(format!("capture-stream-{}", Uuid::new_v4()));
        let spool = base.join("spool");
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        for path in [&base, &spool, &keys, &anchors] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        crate::capture_keyring::rotate(&keys, &anchors, &spool).unwrap();
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'fixture')")
            .bind(user)
            .bind(tenant)
            .execute(&pool)
            .await
            .unwrap();
        let principal = PrincipalContext {
            tenant_id: tenant,
            credential_id: Uuid::nil(),
            credential_name: "fixture".to_owned(),
            user_id: Some(user),
            user_name: None,
            device_id: None,
            device_name: None,
            client_id: None,
            client_name: None,
            agent_id: None,
            agent_name: None,
        };
        let state = AppState::new(test_config(), pool.clone()).unwrap();
        let identity = crate::capture_crypto::CaptureIdentity {
            tenant,
            capture: Uuid::new_v4(),
            user,
            created_ms: Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let session = Arc::new(
            crate::capture_session::AsyncCaptureSession::create_from_keyring(
                &pool,
                crate::capture_session::CapturePaths {
                    spool: &spool,
                    anchors: &anchors,
                },
                &keys,
                identity,
                &[],
                crate::capture_spool::CaptureLimits::for_test(16384),
                Arc::new(tokio::sync::Semaphore::new(4)),
            )
            .await
            .unwrap(),
        );
        session
            .save_requests(b"client-request".to_vec(), b"provider-request".to_vec())
            .await
            .unwrap();
        let mut reservation = state
            .ledger
            .reserve_with_quota(
                RequestStart {
                    request_id: "capture-stream".to_owned(),
                    session_id: None,
                    trace_id: None,
                    requested_model: Some("model".to_owned()),
                    workload_class: None,
                    urgency: None,
                    quality: None,
                    budget_class: None,
                    estimated_tokens: None,
                    selection: None,
                    policy: None,
                    principal: principal.clone(),
                    created_at: Utc::now(),
                },
                Some(identity.capture),
                None,
                None,
            )
            .await
            .unwrap();
        reservation.attach_capture(session.clone());
        let attempt = crate::ledger::ProviderAttempt::begin(
            "provider".to_owned(),
            "model".to_owned(),
            None,
            Utc::now(),
            crate::ledger::TokenUsage::default(),
        );
        const WIRE: &str = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"usage\":{\"input_tokens\":1}}}\n\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":1}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let upstream = Router::new().route(
            "/v1/messages",
            post(|| async { ([("content-type", "text/event-stream")], WIRE) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let response = reqwest::Client::new()
            .post(format!("http://{address}/v1/messages"))
            .send()
            .await
            .unwrap();
        let body = crate::services::streaming::relay(
            response,
            reservation,
            attempt,
            "model".to_owned(),
            "provider".to_owned(),
            crate::services::sse::MetadataParser::default(),
            crate::services::streaming::RelayLimits {
                event_max_bytes: 4096,
                heartbeat_interval: Duration::from_secs(30),
                provider_secret: None,
            },
        )
        .await
        .unwrap();
        let sent = to_bytes(body, 16384).await.unwrap();
        assert_eq!(sent.as_ref(), WIRE.as_bytes());
        drop(session);
        let recorded = crate::capture_spool::CaptureSpool::recover_with_keyring(
            &spool.join(format!("{}.capture", identity.capture)),
            &keys,
            16384,
        )
        .unwrap();
        assert!(recorded.complete);
        let mut provider_bytes = Vec::new();
        let mut client_bytes = Vec::new();
        for (position, (leg, sequence, sealed)) in recorded.blocks.iter().enumerate() {
            if matches!(
                leg,
                crate::capture_crypto::CaptureLeg::ProviderResponse
                    | crate::capture_crypto::CaptureLeg::ClientResponse
            ) {
                let bytes = recorded
                    .cipher
                    .unseal_timed(
                        *leg,
                        *sequence,
                        position as u64,
                        recorded.elapsed_ns[position],
                        sealed,
                    )
                    .unwrap();
                match leg {
                    crate::capture_crypto::CaptureLeg::ProviderResponse => {
                        provider_bytes.extend(bytes)
                    }
                    _ => client_bytes.extend(bytes),
                }
            }
        }
        assert_eq!(provider_bytes, WIRE.as_bytes());
        assert_eq!(client_bytes, sent);
        state.ledger.wait_for_idle().await;
        let ledger_failure_identity = crate::capture_crypto::CaptureIdentity {
            capture: Uuid::new_v4(),
            ..identity
        };
        let ledger_failure = Arc::new(
            crate::capture_session::AsyncCaptureSession::create_from_keyring(
                &pool,
                crate::capture_session::CapturePaths {
                    spool: &spool,
                    anchors: &anchors,
                },
                &keys,
                ledger_failure_identity,
                &[],
                crate::capture_spool::CaptureLimits::for_test(16384),
                Arc::new(tokio::sync::Semaphore::new(4)),
            )
            .await
            .unwrap(),
        );
        ledger_failure
            .save_requests(b"client-request".to_vec(), b"provider-request".to_vec())
            .await
            .unwrap();
        let mut reservation = state
            .ledger
            .reserve_with_quota(
                RequestStart {
                    request_id: "capture-stream-ledger-failure".to_owned(),
                    session_id: None,
                    trace_id: None,
                    requested_model: Some("model".to_owned()),
                    workload_class: None,
                    urgency: None,
                    quality: None,
                    budget_class: None,
                    estimated_tokens: None,
                    selection: None,
                    policy: None,
                    principal: principal.clone(),
                    created_at: Utc::now(),
                },
                Some(ledger_failure_identity.capture),
                None,
                None,
            )
            .await
            .unwrap();
        reservation.attach_capture(ledger_failure.clone());
        let attempt = crate::ledger::ProviderAttempt::begin(
            "provider".to_owned(),
            "model".to_owned(),
            None,
            Utc::now(),
            crate::ledger::TokenUsage::default(),
        );
        let response = reqwest::Client::new()
            .post(format!("http://{address}/v1/messages"))
            .send()
            .await
            .unwrap();
        let body = crate::services::streaming::relay(
            response,
            reservation,
            attempt,
            "model".to_owned(),
            "provider".to_owned(),
            crate::services::sse::MetadataParser::default(),
            crate::services::streaming::RelayLimits {
                event_max_bytes: 4096,
                heartbeat_interval: Duration::from_secs(30),
                provider_secret: None,
            },
        )
        .await
        .unwrap();
        let ledger_dir = &state.config.ledger_spool_dir;
        let held_ledger_dir = ledger_dir.with_extension("held");
        std::fs::rename(ledger_dir, &held_ledger_dir).unwrap();
        let mut output = body.into_data_stream();
        assert!(matches!(output.next().await, Some(Err(_))));
        drop(output);
        state.ledger.wait_for_idle().await;
        std::fs::rename(&held_ledger_dir, ledger_dir).unwrap();
        drop(ledger_failure);
        let incomplete = crate::capture_spool::CaptureSpool::recover_with_keyring(
            &spool.join(format!("{}.capture", ledger_failure_identity.capture)),
            &keys,
            16384,
        )
        .unwrap();
        assert!(!incomplete.complete);
        let cancelled_identity = crate::capture_crypto::CaptureIdentity {
            capture: Uuid::new_v4(),
            ..identity
        };
        let cancelled = Arc::new(
            crate::capture_session::AsyncCaptureSession::create_from_keyring(
                &pool,
                crate::capture_session::CapturePaths {
                    spool: &spool,
                    anchors: &anchors,
                },
                &keys,
                cancelled_identity,
                &[],
                crate::capture_spool::CaptureLimits::for_test(16384),
                Arc::new(tokio::sync::Semaphore::new(4)),
            )
            .await
            .unwrap(),
        );
        cancelled
            .save_requests(b"client-request".to_vec(), b"provider-request".to_vec())
            .await
            .unwrap();
        let mut reservation = state
            .ledger
            .reserve_with_quota(
                RequestStart {
                    request_id: "capture-stream-cancelled".to_owned(),
                    session_id: None,
                    trace_id: None,
                    requested_model: Some("model".to_owned()),
                    workload_class: None,
                    urgency: None,
                    quality: None,
                    budget_class: None,
                    estimated_tokens: None,
                    selection: None,
                    policy: None,
                    principal: principal.clone(),
                    created_at: Utc::now(),
                },
                Some(cancelled_identity.capture),
                None,
                None,
            )
            .await
            .unwrap();
        reservation.attach_capture(cancelled.clone());
        let attempt = crate::ledger::ProviderAttempt::begin(
            "provider".to_owned(),
            "model".to_owned(),
            None,
            Utc::now(),
            crate::ledger::TokenUsage::default(),
        );
        let response = reqwest::Client::new()
            .post(format!("http://{address}/v1/messages"))
            .send()
            .await
            .unwrap();
        let body = crate::services::streaming::relay(
            response,
            reservation,
            attempt,
            "model".to_owned(),
            "provider".to_owned(),
            crate::services::sse::MetadataParser::default(),
            crate::services::streaming::RelayLimits {
                event_max_bytes: 4096,
                heartbeat_interval: Duration::from_secs(30),
                provider_secret: None,
            },
        )
        .await
        .unwrap();
        let mut output = body.into_data_stream();
        assert!(matches!(output.next().await, Some(Ok(_))));
        drop(output);
        drop(cancelled);
        let incomplete = crate::capture_spool::CaptureSpool::recover_with_keyring(
            &spool.join(format!("{}.capture", cancelled_identity.capture)),
            &keys,
            16384,
        )
        .unwrap();
        assert!(!incomplete.complete);
        let failed_identity = crate::capture_crypto::CaptureIdentity {
            capture: Uuid::new_v4(),
            ..identity
        };
        let failed = Arc::new(
            crate::capture_session::AsyncCaptureSession::create_from_keyring(
                &pool,
                crate::capture_session::CapturePaths {
                    spool: &spool,
                    anchors: &anchors,
                },
                &keys,
                failed_identity,
                &[],
                crate::capture_spool::CaptureLimits {
                    per_file: 512,
                    total: 16384 * 16,
                },
                Arc::new(tokio::sync::Semaphore::new(4)),
            )
            .await
            .unwrap(),
        );
        failed
            .save_requests(b"client-request".to_vec(), b"provider-request".to_vec())
            .await
            .unwrap();
        let mut reservation = state
            .ledger
            .reserve_with_quota(
                RequestStart {
                    request_id: "capture-stream-full".to_owned(),
                    session_id: None,
                    trace_id: None,
                    requested_model: Some("model".to_owned()),
                    workload_class: None,
                    urgency: None,
                    quality: None,
                    budget_class: None,
                    estimated_tokens: None,
                    selection: None,
                    policy: None,
                    principal,
                    created_at: Utc::now(),
                },
                Some(failed_identity.capture),
                None,
                None,
            )
            .await
            .unwrap();
        reservation.attach_capture(failed.clone());
        let attempt = crate::ledger::ProviderAttempt::begin(
            "provider".to_owned(),
            "model".to_owned(),
            None,
            Utc::now(),
            crate::ledger::TokenUsage::default(),
        );
        let response = reqwest::Client::new()
            .post(format!("http://{address}/v1/messages"))
            .send()
            .await
            .unwrap();
        let body = crate::services::streaming::relay(
            response,
            reservation,
            attempt,
            "model".to_owned(),
            "provider".to_owned(),
            crate::services::sse::MetadataParser::default(),
            crate::services::streaming::RelayLimits {
                event_max_bytes: 4096,
                heartbeat_interval: Duration::from_secs(30),
                provider_secret: None,
            },
        )
        .await
        .unwrap();
        let mut output = body.into_data_stream();
        assert!(matches!(output.next().await, Some(Err(_))));
        drop(output);
        drop(failed);
        let incomplete = crate::capture_spool::CaptureSpool::recover_with_keyring(
            &spool.join(format!("{}.capture", failed_identity.capture)),
            &keys,
            512,
        )
        .unwrap();
        assert!(!incomplete.complete);
        assert_eq!(incomplete.blocks.len(), 2);
        let error_identity = crate::capture_crypto::CaptureIdentity {
            capture: Uuid::new_v4(),
            ..identity
        };
        let error_session = Arc::new(
            crate::capture_session::AsyncCaptureSession::create_from_keyring(
                &pool,
                crate::capture_session::CapturePaths {
                    spool: &spool,
                    anchors: &anchors,
                },
                &keys,
                error_identity,
                &[],
                crate::capture_spool::CaptureLimits::for_test(16384),
                Arc::new(tokio::sync::Semaphore::new(4)),
            )
            .await
            .unwrap(),
        );
        error_session
            .save_requests(b"client-request".to_vec(), b"provider-request".to_vec())
            .await
            .unwrap();
        let response = reqwest::Client::new()
            .post(format!("http://{address}/v1/messages"))
            .send()
            .await
            .unwrap();
        let event = Bytes::from_static(b"event: error\n\n");
        crate::services::streaming::capture_deferred_error(
            &Some(error_session.clone()),
            Some(response),
            &event,
        )
        .await
        .unwrap();
        crate::services::streaming::finish_capture(&Some(error_session.clone()))
            .await
            .unwrap();
        drop(error_session);
        let recorded_error = crate::capture_spool::CaptureSpool::recover_with_keyring(
            &spool.join(format!("{}.capture", error_identity.capture)),
            &keys,
            16384,
        )
        .unwrap();
        assert!(recorded_error.complete);
        let mut provider_error = Vec::new();
        let mut client_error = Vec::new();
        for (position, (leg, sequence, sealed)) in recorded_error.blocks.iter().enumerate() {
            if matches!(
                leg,
                crate::capture_crypto::CaptureLeg::ProviderResponse
                    | crate::capture_crypto::CaptureLeg::ClientResponse
            ) {
                let bytes = recorded_error
                    .cipher
                    .unseal_timed(
                        *leg,
                        *sequence,
                        position as u64,
                        recorded_error.elapsed_ns[position],
                        sealed,
                    )
                    .unwrap();
                match leg {
                    crate::capture_crypto::CaptureLeg::ProviderResponse => {
                        provider_error.extend(bytes)
                    }
                    _ => client_error.extend(bytes),
                }
            }
        }
        assert_eq!(provider_error, WIRE.as_bytes());
        assert_eq!(client_error, event);
        state.ledger.wait_for_idle().await;
        server.abort();
        std::fs::remove_dir_all(&state.config.ledger_spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn full_capture_records_actual_nonstream_request_and_reply(pool: PgPool) {
        use std::os::unix::fs::PermissionsExt;
        let tenant = Uuid::new_v4();
        let base = std::path::Path::new("/tmp").join(format!("capture-entry-{}", Uuid::new_v4()));
        let spool = base.join("spool");
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        for path in [&base, &spool, &keys, &anchors] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        crate::capture_keyring::rotate(&keys, &anchors, &spool).unwrap();
        let user = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'fixture')")
            .bind(user)
            .bind(tenant)
            .execute(&pool)
            .await
            .unwrap();
        let received = Arc::new(Mutex::new(Vec::<u8>::new()));
        let received_upstream = received.clone();
        let anthropic_received = Arc::new(Mutex::new(Vec::<u8>::new()));
        let anthropic_upstream = anthropic_received.clone();
        const OPENAI_WIRE: &str = concat!(
            "data: {\"id\":\"chat_1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat_1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"chat_1\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n",
            "data: [DONE]\n\n"
        );
        const ANTHROPIC_WIRE: &str = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"type\":\"message\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"answer\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let upstream = Router::new().route("/v1/chat/completions", post(move |body: Bytes| {
            let received = received_upstream.clone();
            async move {
                *received.lock().unwrap() = body.to_vec();
                if body.windows(b"bad-usage".len()).any(|part| part == b"bad-usage") {
                    ([ ("content-type", "application/json") ], r#"{"id":"chat-bad","usage":"invalid"}"#).into_response()
                } else if serde_json::from_slice::<serde_json::Value>(&body).unwrap().get("stream").and_then(serde_json::Value::as_bool) == Some(true) {
                    ([("content-type", "text/event-stream")], OPENAI_WIRE).into_response()
                } else {
                    Json(json!({"id":"chatcmpl-fixture","object":"chat.completion","model":"company-default","choices":[{"index":0,"message":{"role":"assistant","content":"answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})).into_response()
                }
            }
        })).route("/v1/messages", post(move |body: Bytes| {
            let received = anthropic_upstream.clone();
            async move {
                *received.lock().unwrap() = body.to_vec();
                if serde_json::from_slice::<serde_json::Value>(&body).unwrap().get("stream").and_then(serde_json::Value::as_bool) == Some(true) {
                    ([("content-type", "text/event-stream")], ANTHROPIC_WIRE).into_response()
                } else {
                    Json(json!({"id":"msg-fixture","type":"message","role":"assistant","model":"claude-test","content":[{"type":"text","text":"answer"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}})).into_response()
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        sqlx::query(
            r#"WITH provider AS (
                INSERT INTO providers (id, tenant_id, provider_key, display_name, provider_type, base_url, credential_ref, supported_operations, lifecycle_state)
                VALUES (gen_random_uuid(), $1, 'openai-test', 'capture test', 'openai', $2, 'env://OPENAI_API_KEY', ARRAY['openai_chat_completions'], 'active') RETURNING id
            ) INSERT INTO models (id, tenant_id, provider_id, public_model_id, provider_model_id, display_name, supported_operations, supports_streaming, lifecycle_state)
              SELECT gen_random_uuid(), $1, id, 'company-default', 'company-default', 'capture test', ARRAY['openai_chat_completions'], true, 'active' FROM provider"#,
        ).bind(tenant).bind(format!("http://{address}")).execute(&pool).await.unwrap();
        sqlx::query(
            r#"WITH provider AS (
                INSERT INTO providers (id, tenant_id, provider_key, display_name, provider_type, base_url, credential_ref, supported_operations, lifecycle_state)
                VALUES (gen_random_uuid(), $1, 'anthropic-test', 'capture test', 'anthropic', $2, 'env://ANTHROPIC_API_KEY', ARRAY['anthropic_messages'], 'active') RETURNING id
            ) INSERT INTO models (id, tenant_id, provider_id, public_model_id, provider_model_id, display_name, supported_operations, supports_streaming, lifecycle_state)
              SELECT gen_random_uuid(), $1, id, 'claude-test', 'claude-test', 'capture test', ARRAY['anthropic_messages'], true, 'active' FROM provider"#,
        ).bind(tenant).bind(format!("http://{address}")).execute(&pool).await.unwrap();
        let mut config = test_config();
        config.content_capture_enabled = true;
        config.content_capture_spool_dir = spool.clone();
        config.content_capture_key_dir = keys.clone();
        config.content_capture_anchor_dir = anchors;
        config.admin_bootstrap_token = Some("synthetic-bootstrap-token-32-chars".to_owned());
        config.admin_bootstrap_tenant_id = Some(tenant);
        config.openai_base_url = format!("http://{address}");
        config.anthropic_base_url = format!("http://{address}");
        config.allowed_models.push("claude-test".to_owned());
        config.provider_allowed_tenant_ids = vec![tenant];
        let mut state = AppState::new(config, pool).unwrap();
        state.capture_test_enabled = true;
        let erasure = crate::capture_erasure::TestErasure::new();
        state.capture_erasure = Some(erasure.active.clone());
        let state = Arc::new(state);
        let mut caller = principal(tenant);
        caller.user_id = Some(user);
        let input = Bytes::from_static(
            br#"{"model":"company-default","messages":[{"role":"user","content":"capture-private-prompt-sentinel"}]}"#,
        );
        let reply = openai::execute(
            &state,
            RequestContext {
                principal: caller.clone(),
                received_at: Utc::now(),
                path_and_query: "/v1/chat/completions".to_owned(),
                headers: HeaderMap::new(),
                body: input.clone(),
                operation: OpenAiOperation::ChatCompletions,
            },
        )
        .await
        .unwrap();
        assert_eq!(reply.status, StatusCode::OK);
        let output = to_bytes(reply.body, 16384).await.unwrap();
        let path = std::fs::read_dir(&spool)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|value| value == "capture"))
            .unwrap();
        let recorded =
            crate::capture_spool::CaptureSpool::recover_with_keyring(&path, &keys, 16384).unwrap();
        assert!(recorded.complete);
        let mut parts = Vec::new();
        for (position, (leg, sequence, sealed)) in recorded.blocks.iter().take(4).enumerate() {
            parts.push(
                recorded
                    .cipher
                    .unseal_timed(
                        *leg,
                        *sequence,
                        position as u64,
                        recorded.elapsed_ns[position],
                        sealed,
                    )
                    .unwrap(),
            );
        }
        assert_eq!(parts[0], input);
        assert_eq!(parts[1], *received.lock().unwrap());
        assert_eq!(parts[2], output);
        assert_eq!(parts[3], output);
        assert!(
            crate::capture_archive::recover_and_import_keyring(
                &state.pool,
                &path,
                &keys,
                16384,
                Arc::new(tokio::sync::Semaphore::new(1))
            )
            .await
            .unwrap()
        );
        let ip = "127.0.0.1".parse().unwrap();
        let account = state
            .admin
            .bootstrap(
                "synthetic-bootstrap-token-32-chars",
                crate::admin::BootstrapInput {
                    display_name: "admin".to_owned(),
                    credential_name: "fixture".to_owned(),
                },
                ip,
                ip,
                "bootstrap",
            )
            .await
            .unwrap();
        let admin_session = state
            .admin
            .login(&account.login_token, ip, ip, "login")
            .await
            .unwrap();
        let uri = format!(
            "/admin/v1/request-captures/{}/content?leg=client_request",
            recorded.cipher.identity().capture
        );
        let app = router(state.clone());
        let unauthenticated = app
            .clone()
            .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        let logs = logging::capture::Buffer::default();
        let (subscriber, _provider) = logging::test_subscriber(logs.clone());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header(
                        "authorization",
                        format!("Bearer {}", admin_session.access_token),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .with_subscriber(subscriber)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(to_bytes(response.into_body(), 16384).await.unwrap(), input);
        let lines = logs.lines();
        assert!(
            lines
                .iter()
                .any(|line| line["event"]["name"] == "http.access")
        );
        let logged = serde_json::to_string(&lines).unwrap();
        for secret in [
            "capture-private-prompt-sentinel",
            &admin_session.access_token,
        ] {
            assert!(!logged.contains(secret));
            assert!(format!("{logged}{secret}").contains(secret));
        }
        for (direction, expected) in [
            ("provider_request", parts[1].as_slice()),
            ("provider_response", parts[2].as_slice()),
            ("client_response", parts[3].as_slice()),
        ] {
            let url = format!(
                "/admin/v1/request-captures/{}/content?leg={direction}",
                recorded.cipher.identity().capture
            );
            let result = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(url)
                        .header(
                            "authorization",
                            format!("Bearer {}", admin_session.access_token),
                        )
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(result.status(), StatusCode::OK);
            assert_eq!(to_bytes(result.into_body(), 16384).await.unwrap(), expected);
        }
        let invalid = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/admin/v1/request-captures/{}/content?leg=manifest",
                        recorded.cipher.identity().capture
                    ))
                    .header(
                        "authorization",
                        format!("Bearer {}", admin_session.access_token),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let foreign = Uuid::new_v4();
        let foreign_user = Uuid::new_v4();
        let foreign_capture = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'other')")
            .bind(foreign_user)
            .bind(foreign)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO capture_records (tenant_id, capture_id, user_id, key_fingerprint, wrapped_key, expires_at, state) VALUES ($1, $2, $3, $4, $5, now() + interval '30 days', 'complete')")
            .bind(foreign).bind(foreign_capture).bind(foreign_user).bind(vec![1u8; 32]).bind(vec![2u8; 60]).execute(&state.pool).await.unwrap();
        let foreign_result = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/admin/v1/request-captures/{foreign_capture}/content?leg=client_request"
                    ))
                    .header(
                        "authorization",
                        format!("Bearer {}", admin_session.access_token),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(foreign_result.status(), StatusCode::OK);
        let audit_count: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE tenant_id = $1 AND target_id = $2 AND action = 'capture.content.read'")
            .bind(tenant).bind(recorded.cipher.identity().capture).fetch_one(&state.pool).await.unwrap();
        assert_eq!(audit_count, 4);
        sqlx::query("CREATE FUNCTION reject_download_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action = 'capture.content.read' THEN RAISE EXCEPTION 'audit unavailable'; END IF; RETURN NEW; END $$")
            .execute(&state.pool).await.unwrap();
        sqlx::query("CREATE TRIGGER reject_download_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_download_audit()")
            .execute(&state.pool).await.unwrap();
        let failed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header(
                        "authorization",
                        format!("Bearer {}", admin_session.access_token),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(failed.status(), StatusCode::SERVICE_UNAVAILABLE);
        sqlx::query("DROP TRIGGER reject_download_audit ON audit_logs")
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE capture_blocks SET sealed = set_byte(sealed, 12, get_byte(sealed, 12) # 1) WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(tenant).bind(recorded.cipher.identity().capture).execute(&state.pool).await.unwrap();
        let corrupted = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header(
                        "authorization",
                        format!("Bearer {}", admin_session.access_token),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(corrupted.status(), StatusCode::SERVICE_UNAVAILABLE);
        sqlx::query("UPDATE capture_blocks SET sealed = set_byte(sealed, 12, get_byte(sealed, 12) # 1) WHERE tenant_id = $1 AND capture_id = $2 AND position = 0")
            .bind(tenant).bind(recorded.cipher.identity().capture).execute(&state.pool).await.unwrap();
        let anthropic_input = Bytes::from_static(br#"{"model":"claude-test","messages":[{"role":"user","content":"hello"}],"max_tokens":32}"#);
        let anthropic_reply = super::messages::proxy(
            state.clone(),
            caller.clone(),
            Utc::now(),
            "/v1/messages",
            HeaderMap::new(),
            anthropic_input.clone(),
            super::messages::Operation::Messages,
        )
        .await
        .unwrap();
        assert_eq!(anthropic_reply.status(), StatusCode::OK);
        let anthropic_output = to_bytes(anthropic_reply.into_body(), 16384).await.unwrap();
        let anthropic_path = std::fs::read_dir(&spool)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| {
                entry != &path && entry.extension().is_some_and(|value| value == "capture")
            })
            .unwrap();
        let anthropic_recorded =
            crate::capture_spool::CaptureSpool::recover_with_keyring(&anthropic_path, &keys, 16384)
                .unwrap();
        assert!(anthropic_recorded.complete);
        let mut anthropic_parts = Vec::new();
        for (position, (leg, sequence, sealed)) in
            anthropic_recorded.blocks.iter().take(4).enumerate()
        {
            anthropic_parts.push(
                anthropic_recorded
                    .cipher
                    .unseal_timed(
                        *leg,
                        *sequence,
                        position as u64,
                        anthropic_recorded.elapsed_ns[position],
                        sealed,
                    )
                    .unwrap(),
            );
        }
        assert_eq!(anthropic_parts[0], anthropic_input);
        assert_eq!(anthropic_parts[1], *anthropic_received.lock().unwrap());
        assert_eq!(anthropic_parts[2], anthropic_output);
        assert_eq!(anthropic_parts[3], anthropic_output);
        let stream_input = Bytes::from_static(br#"{"model":"claude-test","messages":[{"role":"user","content":"hello"}],"max_tokens":32,"stream":true}"#);
        let stream_reply = super::messages::proxy(
            state.clone(),
            caller.clone(),
            Utc::now(),
            "/v1/messages",
            HeaderMap::new(),
            stream_input.clone(),
            super::messages::Operation::Messages,
        )
        .await
        .unwrap();
        assert_eq!(stream_reply.status(), StatusCode::OK);
        let stream_output = to_bytes(stream_reply.into_body(), 16384).await.unwrap();
        assert_eq!(stream_output, ANTHROPIC_WIRE);
        let stream_path = std::fs::read_dir(&spool)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| {
                entry != &path
                    && entry != &anthropic_path
                    && entry.extension().is_some_and(|value| value == "capture")
            })
            .unwrap();
        let stream_record =
            crate::capture_spool::CaptureSpool::recover_with_keyring(&stream_path, &keys, 16384)
                .unwrap();
        assert!(stream_record.complete);
        let mut legs = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for (position, (leg, sequence, sealed)) in stream_record.blocks.iter().enumerate() {
            let index = match leg {
                crate::capture_crypto::CaptureLeg::ClientRequest => 0,
                crate::capture_crypto::CaptureLeg::ProviderRequest => 1,
                crate::capture_crypto::CaptureLeg::ProviderResponse => 2,
                crate::capture_crypto::CaptureLeg::ClientResponse => 3,
                _ => continue,
            };
            legs[index].extend(
                stream_record
                    .cipher
                    .unseal_timed(
                        *leg,
                        *sequence,
                        position as u64,
                        stream_record.elapsed_ns[position],
                        sealed,
                    )
                    .unwrap(),
            );
        }
        assert_eq!(legs[0], stream_input);
        assert_eq!(legs[1], *anthropic_received.lock().unwrap());
        assert_eq!(legs[2], ANTHROPIC_WIRE.as_bytes());
        assert_eq!(legs[3], stream_output);
        let openai_input = Bytes::from_static(br#"{"model":"company-default","messages":[{"role":"user","content":"hello"}],"stream":true}"#);
        let openai_reply = openai::execute(
            &state,
            RequestContext {
                principal: caller.clone(),
                received_at: Utc::now(),
                path_and_query: "/v1/chat/completions".to_owned(),
                headers: HeaderMap::new(),
                body: openai_input.clone(),
                operation: OpenAiOperation::ChatCompletions,
            },
        )
        .await
        .unwrap();
        let openai_output = to_bytes(openai_reply.body, 16384).await.unwrap();
        assert_eq!(openai_output, OPENAI_WIRE);
        let openai_path = std::fs::read_dir(&spool)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| {
                entry != &path
                    && entry != &anthropic_path
                    && entry != &stream_path
                    && entry.extension().is_some_and(|value| value == "capture")
            })
            .unwrap();
        let openai_record =
            crate::capture_spool::CaptureSpool::recover_with_keyring(&openai_path, &keys, 16384)
                .unwrap();
        assert!(openai_record.complete);
        let mut openai_legs = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for (position, (leg, sequence, sealed)) in openai_record.blocks.iter().enumerate() {
            let index = match leg {
                crate::capture_crypto::CaptureLeg::ClientRequest => 0,
                crate::capture_crypto::CaptureLeg::ProviderRequest => 1,
                crate::capture_crypto::CaptureLeg::ProviderResponse => 2,
                crate::capture_crypto::CaptureLeg::ClientResponse => 3,
                _ => continue,
            };
            openai_legs[index].extend(
                openai_record
                    .cipher
                    .unseal_timed(
                        *leg,
                        *sequence,
                        position as u64,
                        openai_record.elapsed_ns[position],
                        sealed,
                    )
                    .unwrap(),
            );
        }
        assert_eq!(openai_legs[0], openai_input);
        assert_eq!(openai_legs[1], *received.lock().unwrap());
        assert_eq!(openai_legs[2], OPENAI_WIRE.as_bytes());
        assert_eq!(openai_legs[3], openai_output);
        let cross_input = Bytes::from_static(
            br#"{"model":"claude-test","input":"hello","max_output_tokens":32}"#,
        );
        let cross_reply = openai::execute(
            &state,
            RequestContext {
                principal: caller.clone(),
                received_at: Utc::now(),
                path_and_query: "/v1/responses".to_owned(),
                headers: HeaderMap::new(),
                body: cross_input.clone(),
                operation: OpenAiOperation::Responses,
            },
        )
        .await
        .unwrap();
        let cross_output = to_bytes(cross_reply.body, 16384).await.unwrap();
        let cross_path = std::fs::read_dir(&spool)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| {
                entry != &path
                    && entry != &anthropic_path
                    && entry != &stream_path
                    && entry != &openai_path
                    && entry.extension().is_some_and(|value| value == "capture")
            })
            .unwrap();
        let cross_record =
            crate::capture_spool::CaptureSpool::recover_with_keyring(&cross_path, &keys, 16384)
                .unwrap();
        assert!(cross_record.complete);
        let mut cross_parts = Vec::new();
        for (position, (leg, sequence, sealed)) in cross_record.blocks.iter().take(4).enumerate() {
            cross_parts.push(
                cross_record
                    .cipher
                    .unseal_timed(
                        *leg,
                        *sequence,
                        position as u64,
                        cross_record.elapsed_ns[position],
                        sealed,
                    )
                    .unwrap(),
            );
        }
        assert_eq!(cross_parts[0], cross_input);
        assert_eq!(cross_parts[1], *anthropic_received.lock().unwrap());
        assert_ne!(cross_parts[0], cross_parts[1]);
        assert_eq!(cross_parts[2], anthropic_output);
        assert_eq!(cross_parts[3], cross_output);
        assert_ne!(cross_parts[2], cross_parts[3]);
        let cross_stream_input = Bytes::from_static(
            br#"{"model":"claude-test","input":"hello","max_output_tokens":32,"stream":true}"#,
        );
        let cross_stream_reply = openai::execute(
            &state,
            RequestContext {
                principal: caller.clone(),
                received_at: Utc::now(),
                path_and_query: "/v1/responses".to_owned(),
                headers: HeaderMap::new(),
                body: cross_stream_input.clone(),
                operation: OpenAiOperation::Responses,
            },
        )
        .await
        .unwrap();
        let cross_stream_output = to_bytes(cross_stream_reply.body, 65536).await.unwrap();
        assert!(String::from_utf8_lossy(&cross_stream_output).contains("response.completed"));
        let cross_stream_path = std::fs::read_dir(&spool)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| {
                entry != &path
                    && entry != &anthropic_path
                    && entry != &stream_path
                    && entry != &openai_path
                    && entry != &cross_path
                    && entry.extension().is_some_and(|value| value == "capture")
            })
            .unwrap();
        let cross_stream_record = crate::capture_spool::CaptureSpool::recover_with_keyring(
            &cross_stream_path,
            &keys,
            65536,
        )
        .unwrap();
        assert!(cross_stream_record.complete);
        let mut cross_stream_legs = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for (position, (leg, sequence, sealed)) in cross_stream_record.blocks.iter().enumerate() {
            let index = match leg {
                crate::capture_crypto::CaptureLeg::ClientRequest => 0,
                crate::capture_crypto::CaptureLeg::ProviderRequest => 1,
                crate::capture_crypto::CaptureLeg::ProviderResponse => 2,
                crate::capture_crypto::CaptureLeg::ClientResponse => 3,
                _ => continue,
            };
            cross_stream_legs[index].extend(
                cross_stream_record
                    .cipher
                    .unseal_timed(
                        *leg,
                        *sequence,
                        position as u64,
                        cross_stream_record.elapsed_ns[position],
                        sealed,
                    )
                    .unwrap(),
            );
        }
        assert_eq!(cross_stream_legs[0], cross_stream_input);
        assert_eq!(cross_stream_legs[1], *anthropic_received.lock().unwrap());
        assert_eq!(cross_stream_legs[2], ANTHROPIC_WIRE.as_bytes());
        assert_eq!(cross_stream_legs[3], cross_stream_output);
        assert_ne!(cross_stream_legs[0], cross_stream_legs[1]);
        assert_ne!(cross_stream_legs[2], cross_stream_legs[3]);
        let bad_input = Bytes::from_static(
            br#"{"model":"company-default","messages":[{"role":"user","content":"bad-usage"}]}"#,
        );
        let error = match openai::execute(
            &state,
            RequestContext {
                principal: caller.clone(),
                received_at: Utc::now(),
                path_and_query: "/v1/chat/completions".to_owned(),
                headers: HeaderMap::new(),
                body: bad_input.clone(),
                operation: OpenAiOperation::ChatCompletions,
            },
        )
        .await
        {
            Ok(_) => panic!("expected invalid upstream usage to fail"),
            Err(error) => error,
        };
        let error_output = to_bytes(error.into_response().into_body(), 16384)
            .await
            .unwrap();
        let bad_path = std::fs::read_dir(&spool)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| {
                entry != &path
                    && entry != &anthropic_path
                    && entry != &stream_path
                    && entry != &openai_path
                    && entry != &cross_path
                    && entry != &cross_stream_path
                    && entry.extension().is_some_and(|value| value == "capture")
            })
            .unwrap();
        let bad_record =
            crate::capture_spool::CaptureSpool::recover_with_keyring(&bad_path, &keys, 16384)
                .unwrap();
        assert!(bad_record.complete);
        let mut bad_parts = Vec::new();
        for (position, (leg, sequence, sealed)) in bad_record.blocks.iter().take(4).enumerate() {
            bad_parts.push(
                bad_record
                    .cipher
                    .unseal_timed(
                        *leg,
                        *sequence,
                        position as u64,
                        bad_record.elapsed_ns[position],
                        sealed,
                    )
                    .unwrap(),
            );
        }
        assert_eq!(bad_parts[0], bad_input);
        assert_eq!(bad_parts[1], *received.lock().unwrap());
        assert_eq!(bad_parts[2], br#"{"id":"chat-bad","usage":"invalid"}"#);
        assert_eq!(bad_parts[3], error_output);
        let ids = state
            .credentials
            .create_principal(crate::identity::NewPrincipal {
                tenant_id: tenant,
                user_name: "log-fixture",
                device_name: "log-fixture",
                client_name: "log-fixture",
                agent_name: "log-fixture",
            })
            .await
            .unwrap();
        let owner = ids.user_id;
        let gateway_key = state
            .credentials
            .create(crate::identity::NewCredential {
                tenant_id: tenant,
                display_name: "log-fixture",
                principal: ids,
                expires_at: None,
            })
            .await
            .unwrap()
            .plaintext;
        let request_logs = logging::capture::Buffer::default();
        let captures_before = std::fs::read_dir(&spool).unwrap().count();
        for streaming in [false, true] {
            let (subscriber, _provider) = logging::test_subscriber(request_logs.clone());
            let body = json!({
                "model": "company-default",
                "messages": [{"role": "user", "content": "capture-log-request-sentinel"}],
                "stream": streaming,
            });
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/chat/completions")
                        .header("authorization", format!("Bearer {gateway_key}"))
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .with_subscriber(subscriber)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let (subscriber, _provider) = logging::test_subscriber(request_logs.clone());
            let body = to_bytes(response.into_body(), 16384)
                .with_subscriber(subscriber)
                .await
                .unwrap();
            assert!(body.windows(if streaming { 2 } else { 6 }).any(|part| part
                == if streaming {
                    b"ok".as_slice()
                } else {
                    b"answer".as_slice()
                }));
        }
        assert_eq!(
            std::fs::read_dir(&spool).unwrap().count(),
            captures_before + 2
        );
        let request_lines = request_logs.lines();
        assert_eq!(
            request_lines
                .iter()
                .filter(|line| line["event"]["name"] == "http.access")
                .count(),
            2
        );
        let serialized = serde_json::to_string(&request_lines).unwrap();
        for secret in ["capture-log-request-sentinel", gateway_key.as_str()] {
            assert!(!serialized.contains(secret));
        }
        let owned_path = std::fs::read_dir(&spool)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| {
                crate::capture_spool::CaptureSpool::recover_with_keyring(entry, &keys, 16384)
                    .is_ok_and(|record| record.cipher.identity().user == owner)
            })
            .unwrap();
        let owned_record =
            crate::capture_spool::CaptureSpool::recover_with_keyring(&owned_path, &keys, 16384)
                .unwrap();
        let owned_capture = owned_record.cipher.identity().capture;
        assert!(
            crate::capture_archive::recover_and_import_keyring(
                &state.pool,
                &owned_path,
                &keys,
                16384,
                state.capture_slots.clone(),
            )
            .await
            .unwrap()
        );
        let other_ids = state
            .credentials
            .create_principal(crate::identity::NewPrincipal {
                tenant_id: tenant,
                user_name: "other-log-fixture",
                device_name: "other-log-fixture",
                client_name: "other-log-fixture",
                agent_name: "other-log-fixture",
            })
            .await
            .unwrap();
        let other_key = state
            .credentials
            .create(crate::identity::NewCredential {
                tenant_id: tenant,
                display_name: "other-log-fixture",
                principal: other_ids,
                expires_at: None,
            })
            .await
            .unwrap()
            .plaintext;
        let delete_uri = format!("/v1/request-captures/{owned_capture}");
        for (key, expected) in [
            (&other_key, StatusCode::NOT_FOUND),
            (&gateway_key, StatusCode::ACCEPTED),
        ] {
            let result = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(&delete_uri)
                        .header("authorization", format!("Bearer {key}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(result.status(), expected);
        }
        assert!(owned_path.exists());
        let busy_request_slots = state
            .capture_slots
            .clone()
            .acquire_many_owned(16)
            .await
            .unwrap();
        let maintenance = crate::capture_maintenance::CaptureMaintenance::new(
            spool.clone(),
            keys.clone(),
            crate::capture_spool::CaptureLimits::default(),
            erasure.active.clone(),
            0,
        );
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let worker_pool = state.pool.clone();
        let worker = tokio::spawn(async move {
            maintenance
                .run(&worker_pool, Duration::from_millis(50), receiver)
                .await
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            while owned_path.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        drop(busy_request_slots);
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(3), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let result = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(&delete_uri)
                    .header("authorization", format!("Bearer {gateway_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.status(), StatusCode::NO_CONTENT);
        assert!(!owned_path.exists());
        let deleted_content = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/admin/v1/request-captures/{owned_capture}/content?leg=client_request"
                    ))
                    .header(
                        "authorization",
                        format!("Bearer {}", admin_session.access_token),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(deleted_content.status(), StatusCode::OK);
        let previous_upstream = received.lock().unwrap().clone();
        let held_spool = base.join("spool-held");
        std::fs::rename(&spool, &held_spool).unwrap();
        let denied = openai::execute(&state, RequestContext {
            principal: caller, received_at: Utc::now(), path_and_query: "/v1/chat/completions".to_owned(),
            headers: HeaderMap::new(), body: Bytes::from_static(br#"{"model":"company-default","messages":[{"role":"user","content":"must-not-send"}]}"#),
            operation: OpenAiOperation::ChatCompletions,
        }).await;
        std::fs::rename(&held_spool, &spool).unwrap();
        assert!(matches!(
            denied,
            Err(crate::error::GatewayError::CaptureUnavailable)
        ));
        assert_eq!(*received.lock().unwrap(), previous_upstream);
        let large_id = crate::capture_crypto::CaptureIdentity {
            tenant,
            capture: Uuid::new_v4(),
            user,
            created_ms: Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let large_session = crate::capture_session::AsyncCaptureSession::create_from_keyring(
            &state.pool,
            crate::capture_session::CapturePaths {
                spool: &spool,
                anchors: &state.config.content_capture_anchor_dir,
            },
            &keys,
            large_id,
            &[],
            crate::capture_spool::CaptureLimits::default(),
            Arc::new(tokio::sync::Semaphore::new(1)),
        )
        .await
        .unwrap();
        large_session
            .save_requests(vec![b'x'; 8 * 1024 * 1024], b"provider".to_vec())
            .await
            .unwrap();
        large_session
            .save_response(
                crate::capture_crypto::CaptureLeg::ProviderResponse,
                b"response".to_vec(),
            )
            .await
            .unwrap();
        large_session
            .save_response(
                crate::capture_crypto::CaptureLeg::ClientResponse,
                b"response".to_vec(),
            )
            .await
            .unwrap();
        large_session.finish().await.unwrap();
        drop(large_session);
        let large_path = spool.join(format!("{}.capture", large_id.capture));
        assert!(
            crate::capture_archive::recover_and_import_keyring(
                &state.pool,
                &large_path,
                &keys,
                16 * 1024 * 1024,
                Arc::new(tokio::sync::Semaphore::new(1))
            )
            .await
            .unwrap()
        );
        let cancelled = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/admin/v1/request-captures/{}/content?leg=client_request",
                        large_id.capture
                    ))
                    .header(
                        "authorization",
                        format!("Bearer {}", admin_session.access_token),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.status(), StatusCode::OK);
        let mut chunks = cancelled.into_body().into_data_stream();
        assert!(!chunks.next().await.unwrap().unwrap().is_empty());
        drop(chunks);
        let all_permits = tokio::time::timeout(
            Duration::from_secs(5),
            state.capture_slots.clone().acquire_many_owned(16),
        )
        .await
        .unwrap()
        .unwrap();
        drop(all_permits);
        let network_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let network_address = network_listener.local_addr().unwrap();
        let network_server = tokio::spawn(async move {
            axum::serve(
                network_listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let response = reqwest::Client::new()
            .get(format!(
                "http://{network_address}/admin/v1/request-captures/{}/content?leg=client_request",
                large_id.capture
            ))
            .bearer_auth(&admin_session.access_token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut network_chunks = response.bytes_stream();
        let first_chunk = network_chunks.next().await.unwrap().unwrap();
        assert!(!first_chunk.is_empty() && first_chunk.len() < 8 * 1024 * 1024);
        drop(network_chunks);
        let all_permits = tokio::time::timeout(
            Duration::from_secs(10),
            state.capture_slots.clone().acquire_many_owned(16),
        )
        .await
        .unwrap()
        .unwrap();
        drop(all_permits);
        let held_transactions: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND state = 'idle in transaction'")
            .fetch_one(&state.pool).await.unwrap();
        assert_eq!(held_transactions, 0);
        network_server.abort();
        let read_attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE tenant_id = $1 AND target_id = $2 AND action = 'capture.content.read'")
            .bind(tenant).bind(large_id.capture).fetch_one(&state.pool).await.unwrap();
        assert_eq!(read_attempts, 2);
        state.ledger.wait_for_idle().await;
        server.abort();
        std::fs::remove_dir_all(&state.config.ledger_spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn capture_report_requires_admin_and_same_tenant(pool: PgPool) {
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        let capture = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'fixture')")
            .bind(user)
            .bind(tenant)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO capture_records (tenant_id, capture_id, user_id, key_fingerprint, wrapped_key, expires_at, state) VALUES ($1, $2, $3, $4, $5, now() + interval '30 days', 'complete')")
            .bind(tenant).bind(capture).bind(user).bind(vec![1u8; 32]).bind(vec![2u8; 60]).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO capture_blocks (tenant_id, capture_id, leg, sequence, position, elapsed_ns, sealed) VALUES ($1, $2, 0, 0, 0, 0, $3), ($1, $2, 0, 1, 1, 1, $3), ($1, $2, 2, 0, 2, 2, $3)")
            .bind(tenant).bind(capture).bind(vec![1u8; 28]).execute(&pool).await.unwrap();
        let mut config = test_config();
        config.admin_bootstrap_token = Some("synthetic-bootstrap-token-32-chars".to_owned());
        config.admin_bootstrap_tenant_id = Some(tenant);
        let state = AppState::new(config, pool).unwrap();
        let ip = "127.0.0.1".parse().unwrap();
        let account = state
            .admin
            .bootstrap(
                "synthetic-bootstrap-token-32-chars",
                crate::admin::BootstrapInput {
                    display_name: "admin".to_owned(),
                    credential_name: "fixture".to_owned(),
                },
                ip,
                ip,
                "bootstrap",
            )
            .await
            .unwrap();
        let session = state
            .admin
            .login(&account.login_token, ip, ip, "login")
            .await
            .unwrap();
        let context = state
            .admin
            .resolve_session(&session.access_token)
            .await
            .unwrap();
        let report = state.admin.capture_report(&context, capture).await.unwrap();
        assert_eq!(report["id"], capture.to_string());
        assert_eq!(report["user_id"], user.to_string());
        assert_eq!(report["state"], "complete");
        assert_eq!(
            report["blocks"],
            json!({"client_request":2,"provider_request":0,"provider_response":1,"client_response":0})
        );
        assert!(report.get("wrapped_key").is_none() && report.get("key_fingerprint").is_none());
        let mut denied = context.clone();
        denied.tenant_id = Uuid::new_v4();
        assert!(matches!(
            state.admin.capture_report(&denied, capture).await,
            Err(crate::admin::AdminError::Unauthorized)
        ));
        denied.tenant_id = tenant;
        denied.role = crate::admin::AdminRole::Viewer;
        assert!(matches!(
            state.admin.capture_report(&denied, capture).await,
            Err(crate::admin::AdminError::Forbidden)
        ));
        assert!(
            crate::admin::authorize("GET", "/admin/v1/request-captures/{id}", denied.role)
                .is_none()
        );
        assert!(
            crate::admin::authorize("GET", "/admin/v1/request-captures/{id}", context.role)
                .is_some()
        );
        let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE tenant_id = $1 AND actor_admin_id = $2 AND target_id = $3 AND action = 'capture.metadata.read'")
            .bind(tenant).bind(context.account_id).bind(capture).fetch_one(&state.pool).await.unwrap();
        assert_eq!(audits, 1);
        sqlx::query("CREATE FUNCTION reject_capture_metadata_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action = 'capture.metadata.read' THEN RAISE EXCEPTION 'audit unavailable'; END IF; RETURN NEW; END $$")
            .execute(&state.pool).await.unwrap();
        sqlx::query("CREATE TRIGGER reject_capture_metadata_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_capture_metadata_audit()")
            .execute(&state.pool).await.unwrap();
        assert!(matches!(
            state.admin.capture_report(&context, capture).await,
            Err(crate::admin::AdminError::Database(_))
        ));
        sqlx::query("UPDATE admin_sessions SET revoked_at = now() WHERE id = $1")
            .bind(context.session_id)
            .execute(&state.pool)
            .await
            .unwrap();
        assert!(matches!(
            state.admin.capture_report(&context, capture).await,
            Err(crate::admin::AdminError::Unauthorized)
        ));
        std::fs::remove_dir_all(&state.config.ledger_spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn unavailable_content_capture_stops_before_provider(pool: PgPool) {
        let called = Arc::new(AtomicUsize::new(0));
        let upstream_calls = called.clone();
        let anthropic_calls = called.clone();
        let upstream = Router::new()
            .route(
                "/v1/chat/completions",
                post(move || {
                    let counter = upstream_calls.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Json(json!({}))
                    }
                }),
            )
            .route(
                "/v1/messages",
                post(move || {
                    let counter = anthropic_calls.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Json(json!({}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.content_capture_enabled = true;
        config.openai_base_url = format!("http://{address}");
        config.anthropic_base_url = format!("http://{address}");
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
        let response = router(state.clone()).oneshot(Request::builder().method("POST")
            .uri("/v1/chat/completions").header("authorization", "Bearer client-key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model":"company-default","messages":[{"role":"user","content":"hello"}]}"#)).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get("retry-after").unwrap(), "1");
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "capture_unavailable");
        let anthropic = router(state.clone()).oneshot(Request::builder().method("POST")
            .uri("/v1/messages").header("x-api-key", "client-key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model":"company-default","messages":[{"role":"user","content":"hello"}],"max_tokens":10}"#)).unwrap()).await.unwrap();
        assert_eq!(anthropic.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(anthropic.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "capture_unavailable");
        assert_eq!(called.load(Ordering::SeqCst), 0);
        state.ledger.wait_for_idle().await;
        state.ledger.flush_startup().await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM gateway_requests WHERE status='rejected' AND policy_decision->>'denial_reason'='capture_unavailable'").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 2);
        server.abort();
        std::fs::remove_dir_all(&state.config.ledger_spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn admin_login_and_http_task_filter_keep_tenants_separate(pool: PgPool) {
        let tenant = Uuid::new_v4();
        let other = Uuid::new_v4();
        let owner = Uuid::new_v4();
        let foreign_owner = Uuid::new_v4();
        let own_capture = Uuid::new_v4();
        let foreign_capture = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'mine'), ($3, $4, 'other')")
            .bind(owner).bind(tenant).bind(foreign_owner).bind(other).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO capture_records (tenant_id, capture_id, user_id, key_fingerprint, wrapped_key, expires_at, state) VALUES ($1, $2, $3, $4, $5, now() + interval '30 days', 'complete'), ($6, $7, $8, $4, $5, now() + interval '30 days', 'complete')")
            .bind(tenant).bind(own_capture).bind(owner).bind(vec![1u8; 32]).bind(vec![2u8; 60]).bind(other).bind(foreign_capture).bind(foreign_owner).execute(&pool).await.unwrap();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        sqlx::query("INSERT INTO gateway_requests (id, tenant_id, request_id, status, created_at, completed_at) VALUES ($1, $2, 'mine', 'completed', now(), now()), ($3, $4, 'other', 'completed', now(), now())")
            .bind(first).bind(tenant).bind(second).bind(other).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO request_attribution (tenant_id, gateway_request_id, task_id) VALUES ($1, $2, 'same-task'), ($3, $4, 'same-task')")
            .bind(tenant).bind(first).bind(other).bind(second).execute(&pool).await.unwrap();
        let mut config = test_config();
        config.admin_bootstrap_token = Some("synthetic-bootstrap-token-32-chars".to_owned());
        config.admin_bootstrap_tenant_id = Some(tenant);
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let app = router(state.clone());
        let peer: std::net::SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let request = Request::builder()
            .method("POST")
            .uri("/admin/v1/bootstrap")
            .header("authorization", "Bearer synthetic-bootstrap-token-32-chars")
            .header("content-type", "application/json")
            .extension(axum::extract::ConnectInfo(peer))
            .body(Body::from(
                r#"{"display_name":"test","credential_name":"test"}"#,
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let issued: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        let login = Request::builder()
            .method("POST")
            .uri("/admin/v1/sessions")
            .header(
                "authorization",
                format!("Bearer {}", issued["login_token"].as_str().unwrap()),
            )
            .extension(axum::extract::ConnectInfo(peer))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(login).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let session: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        let from = (Utc::now() - chrono::Duration::days(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let to = (Utc::now() + chrono::Duration::minutes(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let uri = format!("/admin/v1/requests?from={from}&to={to}&task_id=same-task");
        let unauthorized = app
            .clone()
            .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let request = Request::builder()
            .uri(&uri)
            .header(
                "authorization",
                format!("Bearer {}", session["access_token"].as_str().unwrap()),
            )
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["items"][0]["id"], first.to_string());
        assert_eq!(page["items"][0]["task_id"], "same-task");
        assert!(page["items"][0]["completed_at"].is_string());
        let token = format!("Bearer {}", session["access_token"].as_str().unwrap());
        let capture_uri = format!("/admin/v1/request-captures/{own_capture}");
        let owner_context = state
            .admin
            .resolve_session(session["access_token"].as_str().unwrap())
            .await
            .unwrap();
        for role in [
            crate::admin::AdminRole::Viewer,
            crate::admin::AdminRole::Operator,
        ] {
            let account = state
                .admin
                .create_admin_account(
                    &owner_context,
                    crate::admin::AdminAccountCreate {
                        display_name: role.as_str().to_owned(),
                        credential_name: "fixture".to_owned(),
                        role,
                    },
                    peer.ip(),
                    peer.ip(),
                    "create-restricted-account",
                )
                .await
                .unwrap();
            let restricted = state
                .admin
                .login(
                    &account.login_token,
                    peer.ip(),
                    peer.ip(),
                    "restricted-login",
                )
                .await
                .unwrap();
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(&capture_uri)
                        .header(
                            "authorization",
                            format!("Bearer {}", restricted.access_token),
                        )
                        .extension(axum::extract::ConnectInfo(peer))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            let content = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/admin/v1/request-captures/{own_capture}/content?leg=client_request"
                        ))
                        .header(
                            "authorization",
                            format!("Bearer {}", restricted.access_token),
                        )
                        .extension(axum::extract::ConnectInfo(peer))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(content.status(), StatusCode::FORBIDDEN);
        }
        let denials: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE tenant_id = $1 AND action = 'admin.authorization.denied' AND target_type = 'admin_route'")
            .bind(tenant).fetch_one(&state.pool).await.unwrap();
        assert_eq!(denials, 4);
        let no_session = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&capture_uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(no_session.status(), StatusCode::UNAUTHORIZED);
        let own_capture_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&capture_uri)
                    .header("authorization", &token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(own_capture_response.status(), StatusCode::OK);
        let capture_report: serde_json::Value = serde_json::from_slice(
            &to_bytes(own_capture_response.into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(capture_report["user_id"], owner.to_string());
        assert!(capture_report.get("wrapped_key").is_none());
        let foreign_capture_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/admin/v1/request-captures/{foreign_capture}"))
                    .header("authorization", &token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(foreign_capture_response.status(), StatusCode::NOT_FOUND);
        let own = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/admin/v1/requests/{first}"))
                    .header("authorization", &token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(own.status(), StatusCode::OK);
        let detail: serde_json::Value =
            serde_json::from_slice(&to_bytes(own.into_body(), 8192).await.unwrap()).unwrap();
        assert_eq!(detail["attribution"]["task_id"], "same-task");
        let foreign = app
            .oneshot(
                Request::builder()
                    .uri(format!("/admin/v1/requests/{second}"))
                    .header("authorization", &token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
        std::fs::remove_dir_all(&state.config.ledger_spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn policy_denial_is_durable_and_never_reaches_model_or_provider(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO policies (
                id, tenant_id, policy_key, revision, configuration, lifecycle_state
            ) VALUES ($1, $2, 'global-deny', 1,
                '{"deny":true,"deny_reason":"maintenance"}', 'active')
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .execute(&pool)
        .await
        .unwrap();
        let mut config = test_config();
        let spool_dir = config.ledger_spool_dir.clone();
        config.provider_allowed_tenant_ids = vec![tenant_id];
        let state = AppState::new(config, pool.clone()).unwrap();
        let result = openai::execute(
            &state,
            RequestContext {
                principal: PrincipalContext {
                    tenant_id,
                    credential_id: Uuid::nil(),
                    credential_name: "policy-test".to_owned(),
                    user_id: None,
                    user_name: None,
                    device_id: None,
                    device_name: None,
                    client_id: None,
                    client_name: None,
                    agent_id: None,
                    agent_name: None,
                },
                received_at: Utc::now(),
                path_and_query: "/v1/chat/completions".to_owned(),
                headers: HeaderMap::new(),
                body: Bytes::from_static(br#"{"model":"not-in-registry"}"#),
                operation: OpenAiOperation::ChatCompletions,
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(crate::error::GatewayError::PolicyDenied)
        ));
        state.ledger.wait_for_idle().await;
        state.ledger.flush_startup().await.unwrap();
        let evidence: serde_json::Value = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'status', gr.status,
                'quality', gr.quality,
                'denied', gr.policy_decision->'denied',
                'reason', gr.policy_decision->'denial_reason',
                'attempts', (SELECT count(*) FROM provider_attempts pa
                    WHERE (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id))
            )
            FROM gateway_requests gr WHERE gr.tenant_id = $1
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(evidence["status"], "rejected");
        assert_eq!(evidence["quality"], "standard");
        assert_eq!(evidence["denied"], true);
        assert_eq!(evidence["reason"], "maintenance");
        assert_eq!(evidence["attempts"], 0);
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    /// The same guarantee for the other reason a request never reaches the
    /// policy: the model field is there but empty, so there is no name to
    /// resolve. The ledger has to show which model string was refused.
    #[sqlx::test]
    async fn a_request_with_an_unparsable_model_name_is_still_recorded(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let mut config = test_config();
        let spool_dir = config.ledger_spool_dir.clone();
        config.provider_allowed_tenant_ids = vec![tenant_id];
        let state = AppState::new(config, pool.clone()).unwrap();
        let result = openai::execute(
            &state,
            RequestContext {
                principal: PrincipalContext {
                    tenant_id,
                    credential_id: Uuid::nil(),
                    credential_name: "selector-test".to_owned(),
                    user_id: None,
                    user_name: None,
                    device_id: None,
                    device_name: None,
                    client_id: None,
                    client_name: None,
                    agent_id: None,
                    agent_name: None,
                },
                received_at: Utc::now(),
                path_and_query: "/v1/chat/completions".to_owned(),
                headers: HeaderMap::new(),
                body: Bytes::from_static(br#"{"model":""}"#),
                operation: OpenAiOperation::ChatCompletions,
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(crate::error::GatewayError::ModelForbidden)
        ));
        state.ledger.wait_for_idle().await;
        state.ledger.flush_startup().await.unwrap();
        let evidence: serde_json::Value = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'status', gr.status,
                'reason', gr.policy_decision->'denial_reason',
                'evaluated', gr.policy_decision->'evaluated'
            )
            FROM gateway_requests gr WHERE gr.tenant_id = $1
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(evidence["status"], "rejected");
        assert_eq!(evidence["reason"], "model_name_unparsable");
        assert_eq!(evidence["evaluated"], false);
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    /// Recording is best effort: a ledger that cannot write must not change the
    /// answer to a malformed request, because a 503 makes clients retry the
    /// requests that are already the problem.
    #[sqlx::test]
    async fn an_unwritable_ledger_does_not_change_a_malformed_request_answer(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let mut config = test_config();
        // A spool with no room at all: every reservation fails.
        config.ledger_spool_dir = std::env::temp_dir().join(format!(
            "agent-pool-gateway-full-spool-{}",
            uuid::Uuid::new_v4()
        ));
        let spool_dir = config.ledger_spool_dir.clone();
        config.ledger_spool_max_bytes = config.ledger_spool_entry_max_bytes * 3;
        config.provider_allowed_tenant_ids = vec![tenant_id];
        let state = AppState::new(config, pool.clone()).unwrap();
        // Fill the single slot, so the next reservation has nowhere to go.
        let _held = state
            .ledger
            .reserve(crate::ledger::RequestStart {
                request_id: "holder".to_owned(),
                session_id: None,
                trace_id: None,
                requested_model: None,
                workload_class: None,
                urgency: None,
                quality: None,
                budget_class: None,
                estimated_tokens: None,
                selection: None,
                policy: None,
                principal: PrincipalContext {
                    tenant_id,
                    credential_id: Uuid::nil(),
                    credential_name: "holder".to_owned(),
                    user_id: None,
                    user_name: None,
                    device_id: None,
                    device_name: None,
                    client_id: None,
                    client_name: None,
                    agent_id: None,
                    agent_name: None,
                },
                created_at: Utc::now(),
            })
            .await
            .unwrap();

        let result = openai::execute(
            &state,
            RequestContext {
                principal: PrincipalContext {
                    tenant_id,
                    credential_id: Uuid::nil(),
                    credential_name: "full-spool-test".to_owned(),
                    user_id: None,
                    user_name: None,
                    device_id: None,
                    device_name: None,
                    client_id: None,
                    client_name: None,
                    agent_id: None,
                    agent_name: None,
                },
                received_at: Utc::now(),
                path_and_query: "/v1/chat/completions".to_owned(),
                headers: HeaderMap::new(),
                body: Bytes::from_static(b"not json"),
                operation: OpenAiOperation::ChatCompletions,
            },
        )
        .await;
        assert!(
            matches!(result, Err(crate::error::GatewayError::InvalidRequest(_))),
            "a full spool must not turn a malformed request into a 503"
        );
        drop(_held);
        state.ledger.wait_for_finalizations().await;
        state.ledger.wait_for_idle().await;
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    /// A body that does not decode is refused before any policy runs. It still
    /// arrived from a known credential, so Admin has to be able to say it
    /// happened -- "nothing in the ledger" and "the request never reached us"
    /// must not look the same.
    #[sqlx::test]
    async fn a_request_that_does_not_decode_is_still_recorded(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let mut config = test_config();
        let spool_dir = config.ledger_spool_dir.clone();
        config.provider_allowed_tenant_ids = vec![tenant_id];
        let state = AppState::new(config, pool.clone()).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-apg-task-id", "invalid-request-task".parse().unwrap());
        let result = openai::execute(
            &state,
            RequestContext {
                principal: PrincipalContext {
                    tenant_id,
                    credential_id: Uuid::nil(),
                    credential_name: "decode-test".to_owned(),
                    user_id: None,
                    user_name: None,
                    device_id: None,
                    device_name: None,
                    client_id: None,
                    client_name: None,
                    agent_id: None,
                    agent_name: None,
                },
                received_at: Utc::now(),
                path_and_query: "/v1/chat/completions".to_owned(),
                headers,
                body: Bytes::from_static(b"this is not json at all"),
                operation: OpenAiOperation::ChatCompletions,
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(crate::error::GatewayError::InvalidRequest(_))
        ));
        state.ledger.wait_for_idle().await;
        state.ledger.flush_startup().await.unwrap();
        let evidence: serde_json::Value = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'status', gr.status,
                'reason', gr.policy_decision->'denial_reason',
                'evaluated', gr.policy_decision->'evaluated',
                'operation', gr.operation,
                'task_id', (SELECT a.task_id FROM request_attribution a
                    WHERE (a.tenant_id, a.gateway_request_id) = (gr.tenant_id, gr.id)),
                'attempts', (SELECT count(*) FROM provider_attempts pa
                    WHERE (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id))
            )
            FROM gateway_requests gr WHERE gr.tenant_id = $1
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(evidence["status"], "rejected");
        assert_eq!(evidence["reason"], "request_undecodable");
        // Not an empty decision that reads like a policy allowed it.
        assert_eq!(evidence["evaluated"], false);
        assert_eq!(evidence["operation"], "chat_completions");
        assert_eq!(evidence["task_id"], "invalid-request-task");
        assert_eq!(evidence["attempts"], 0, "the provider was never called");
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    /// The rejection an operator meets most often: the registry has nothing to
    /// offer yet. Every other rejection leaves a row, so this one must too --
    /// otherwise "my request fails" has no trace in Admin at all.
    #[sqlx::test]
    async fn a_request_with_no_model_available_is_still_recorded(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let mut config = test_config();
        let spool_dir = config.ledger_spool_dir.clone();
        config.provider_allowed_tenant_ids = vec![tenant_id];
        let state = AppState::new(config, pool.clone()).unwrap();
        let result = openai::execute(
            &state,
            RequestContext {
                principal: PrincipalContext {
                    tenant_id,
                    credential_id: Uuid::nil(),
                    credential_name: "registry-test".to_owned(),
                    user_id: None,
                    user_name: None,
                    device_id: None,
                    device_name: None,
                    client_id: None,
                    client_name: None,
                    agent_id: None,
                    agent_name: None,
                },
                received_at: Utc::now(),
                path_and_query: "/v1/chat/completions".to_owned(),
                headers: HeaderMap::new(),
                body: Bytes::from_static(br#"{"model":"nothing-is-registered"}"#),
                operation: OpenAiOperation::ChatCompletions,
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(crate::error::GatewayError::ModelForbidden)
        ));
        state.ledger.wait_for_idle().await;
        state.ledger.flush_startup().await.unwrap();
        let evidence: serde_json::Value = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'status', gr.status,
                'requested', gr.requested_model,
                'reason', gr.policy_decision->'denial_reason',
                'attempts', (SELECT count(*) FROM provider_attempts pa
                    WHERE (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id))
            )
            FROM gateway_requests gr WHERE gr.tenant_id = $1
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(evidence["status"], "rejected");
        assert_eq!(evidence["requested"], "nothing-is-registered");
        assert_eq!(evidence["reason"], "no_model_available");
        assert_eq!(evidence["attempts"], 0, "the provider was never called");
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn hard_quota_is_a_durable_429_before_the_provider_call(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let provider_id = Uuid::new_v4();
        let model_id = Uuid::new_v4();
        sqlx::query(
            r#"
            WITH inserted_provider AS (
                INSERT INTO providers (
                    id, tenant_id, provider_key, display_name, provider_type,
                    base_url, credential_ref, supported_operations, lifecycle_state
                ) VALUES (
                    $1, $2, 'openai-test', 'Quota provider', 'openai',
                    'http://127.0.0.1:1', 'env://OPENAI_API_KEY',
                    ARRAY['openai_chat_completions'], 'active'
                ) RETURNING id
            ), inserted_model AS (
                INSERT INTO models (
                    id, tenant_id, provider_id, public_model_id, provider_model_id,
                    display_name, supported_operations, lifecycle_state
                ) SELECT $3, $2, id, 'quota-model', 'provider-quota-model',
                         'Quota model', ARRAY['openai_chat_completions'], 'active'
                  FROM inserted_provider RETURNING id
            )
            INSERT INTO quotas (
                id, tenant_id, quota_key, revision, configuration, lifecycle_state
            ) SELECT $4, $2, 'closed', 1,
                     '{"mode":"hard","limits":{"requests_per_minute":0}}', 'active'
              FROM inserted_model
            "#,
        )
        .bind(provider_id)
        .bind(tenant_id)
        .bind(model_id)
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();
        let mut config = test_config();
        config.provider_allowed_tenant_ids = vec![tenant_id];
        let spool_dir = config.ledger_spool_dir.clone();
        let state = AppState::new(config, pool.clone()).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-apg-task-id", "quota-task".parse().unwrap());
        let result = openai::execute(
            &state,
            RequestContext {
                principal: PrincipalContext {
                    tenant_id,
                    credential_id: Uuid::nil(),
                    credential_name: "quota-test".to_owned(),
                    user_id: None,
                    user_name: None,
                    device_id: None,
                    device_name: None,
                    client_id: None,
                    client_name: None,
                    agent_id: None,
                    agent_name: None,
                },
                received_at: Utc::now(),
                path_and_query: "/v1/chat/completions".to_owned(),
                headers,
                body: Bytes::from_static(br#"{"model":"quota-model","max_tokens":10}"#),
                operation: OpenAiOperation::ChatCompletions,
            },
        )
        .await;
        let Err(error) = result else {
            panic!("closed hard quota reached the provider path");
        };
        assert_eq!(error.kind(), "quota_exceeded");
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "quota_exceeded");
        state.ledger.wait_for_idle().await;
        state.ledger.flush_startup().await.unwrap();
        let evidence: serde_json::Value = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'status', gr.status,
                'reason', gr.quota_decision->'denial_reason',
                'limit_kind', gr.quota_decision->'violations'->0->'limit_kind',
                'task_id', (SELECT a.task_id FROM request_attribution a
                    WHERE (a.tenant_id, a.gateway_request_id) = (gr.tenant_id, gr.id)),
                'attempts', (SELECT count(*) FROM provider_attempts pa
                    WHERE (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id))
            )
            FROM gateway_requests gr WHERE gr.tenant_id = $1
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(evidence["status"], "rejected");
        assert_eq!(evidence["reason"], "hard_limit");
        assert_eq!(evidence["limit_kind"], "requests_per_minute");
        assert_eq!(evidence["task_id"], "quota-task");
        assert_eq!(evidence["attempts"], 0);
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn rejected_cross_provider_reasoning_never_calls_upstream_or_reserves_quota(
        pool: PgPool,
    ) {
        let called = Arc::new(AtomicBool::new(false));
        let called_by_server = Arc::clone(&called);
        let upstream = Router::new().route(
            "/v1/messages",
            post(move || {
                let called = Arc::clone(&called_by_server);
                async move {
                    called.store(true, Ordering::Release);
                    Json(json!({
                        "id": "must-not-exist",
                        "content": [],
                        "usage": {"input_tokens": 1, "output_tokens": 1}
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let tenant_id = Uuid::new_v4();
        sqlx::query(
            r#"
            WITH inserted_provider AS (
                INSERT INTO providers (
                    id, tenant_id, provider_key, display_name, provider_type,
                    base_url, credential_ref, supported_operations, lifecycle_state
                ) VALUES (
                    $1, $2, 'anthropic-test', 'Cross provider', 'anthropic',
                    $3, 'env://ANTHROPIC_API_KEY', ARRAY['anthropic_messages'], 'active'
                ) RETURNING id
            ), inserted_model AS (
                INSERT INTO models (
                    id, tenant_id, provider_id, public_model_id, provider_model_id,
                    display_name, supported_operations, supports_reasoning,
                    max_output_tokens, lifecycle_state
                ) SELECT $4, $2, id, 'cross-model', 'provider-cross-model',
                         'Cross model', ARRAY['anthropic_messages'], true, 4096, 'active'
                  FROM inserted_provider RETURNING id
            )
            INSERT INTO quotas (
                id, tenant_id, quota_key, revision, configuration, lifecycle_state
            ) SELECT $5, $2, 'cross-test', 1,
                     '{"mode":"hard","limits":{"requests_per_minute":100}}', 'active'
              FROM inserted_model
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(format!("http://{address}"))
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();

        let mut config = test_config();
        config.anthropic_base_url = format!("http://{address}");
        config.provider_allowed_tenant_ids = vec![tenant_id];
        let spool_dir = config.ledger_spool_dir.clone();
        let state = AppState::new(config, pool.clone()).unwrap();
        let result = openai::execute(
            &state,
            RequestContext {
                principal: principal(tenant_id),
                received_at: Utc::now(),
                path_and_query: "/v1/responses".to_owned(),
                headers: HeaderMap::new(),
                body: Bytes::from_static(
                    br#"{"model":"cross-model","input":"hello","max_output_tokens":32,"reasoning":{"effort":"high"}}"#,
                ),
                operation: OpenAiOperation::Responses,
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(crate::error::GatewayError::UnsupportedRequest)
        ));
        assert!(
            !called.load(Ordering::Acquire),
            "rejected request called upstream"
        );
        let quota_counts: serde_json::Value = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'windows', (SELECT count(*) FROM quota_windows WHERE tenant_id = $1),
                'reservations', (SELECT count(*) FROM quota_reservations WHERE tenant_id = $1)
            )
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(quota_counts, json!({"windows": 0, "reservations": 0}));

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn buffered_text_crosses_both_protocols_with_gateway_ids_and_trusted_headers(
        pool: PgPool,
    ) {
        let seen = Arc::new(Mutex::new(
            Vec::<(String, HeaderMap, serde_json::Value)>::new(),
        ));
        let openai_seen = Arc::clone(&seen);
        let anthropic_seen = Arc::clone(&seen);
        let upstream = Router::new()
            .route(
                "/v1/responses",
                post(move |headers: HeaderMap, body: Bytes| {
                    let seen = Arc::clone(&openai_seen);
                    async move {
                        seen.lock().unwrap().push((
                            "openai".to_owned(),
                            headers,
                            serde_json::from_slice(&body).unwrap(),
                        ));
                        Json(json!({
                            "id": "resp_target_private",
                            "object": "response",
                            "status": "incomplete",
                            "incomplete_details": {"reason": "max_output_tokens"},
                            "model": "provider-openai-model",
                            "output": [{"type":"message","id":"item_target","status":"completed","role":"assistant","content":[{"type":"output_text","text":"from openai","annotations":[]}]}],
                            "usage": {"input_tokens": 7, "output_tokens": 2}
                        }))
                    }
                }),
            )
            .route(
                "/v1/messages",
                post(move |headers: HeaderMap, body: Bytes| {
                    let seen = Arc::clone(&anthropic_seen);
                    async move {
                        seen.lock().unwrap().push((
                            "anthropic".to_owned(),
                            headers,
                            serde_json::from_slice(&body).unwrap(),
                        ));
                        Json(json!({
                            "id": "msg_target_private",
                            "type": "message",
                            "role": "assistant",
                            "model": "provider-anthropic-model",
                            "content": [{"type":"text","text":"from anthropic"}],
                            "stop_reason": "max_tokens",
                            "usage": {"input_tokens": 9, "output_tokens": 3}
                        }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let tenant_id = Uuid::new_v4();
        let base_url = format!("http://{address}");
        sqlx::query(
            r#"
            WITH inserted_providers AS (
                INSERT INTO providers (
                    id, tenant_id, provider_key, display_name, provider_type,
                    base_url, credential_ref, supported_operations, lifecycle_state
                ) VALUES
                    ($1, $3, 'openai-test', 'OpenAI target', 'openai', $4,
                     'env://OPENAI_API_KEY', ARRAY['openai_responses'], 'active'),
                    ($2, $3, 'anthropic-test', 'Anthropic target', 'anthropic', $4,
                     'env://ANTHROPIC_API_KEY', ARRAY['anthropic_messages'], 'active')
                RETURNING id, provider_key
            )
            INSERT INTO models (
                id, tenant_id, provider_id, public_model_id, provider_model_id,
                display_name, supported_operations, lifecycle_state
            )
            SELECT gen_random_uuid(), $3, id,
                   CASE provider_key WHEN 'openai-test' THEN 'openai-cross' ELSE 'anthropic-cross' END,
                   CASE provider_key WHEN 'openai-test' THEN 'provider-openai-model' ELSE 'provider-anthropic-model' END,
                   'Cross model',
                   CASE provider_key WHEN 'openai-test' THEN ARRAY['openai_responses'] ELSE ARRAY['anthropic_messages'] END,
                   'active'
            FROM inserted_providers
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(&base_url)
        .execute(&pool)
        .await
        .unwrap();

        let mut config = test_config();
        config.openai_base_url = base_url.clone();
        config.anthropic_base_url = base_url;
        config.provider_allowed_tenant_ids = vec![tenant_id];
        let spool_dir = config.ledger_spool_dir.clone();
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
        let openai_reply = openai::execute(
            &state,
            RequestContext {
                principal: principal(tenant_id),
                received_at: Utc::now(),
                path_and_query: "/v1/responses?client=true".to_owned(),
                headers: HeaderMap::from_iter([(
                    "openai-beta".parse().unwrap(),
                    "client-beta".parse().unwrap(),
                )]),
                body: Bytes::from_static(
                    br#"{"model":"anthropic-cross","input":"hello","max_output_tokens":32}"#,
                ),
                operation: OpenAiOperation::Responses,
            },
        )
        .await
        .unwrap();
        assert_eq!(openai_reply.status, StatusCode::OK);
        assert!(openai_reply.headers.get("anthropic-version").is_none());
        let openai_body: serde_json::Value =
            serde_json::from_slice(&to_bytes(openai_reply.body, 64 * 1024).await.unwrap()).unwrap();
        assert!(openai_body["id"].as_str().unwrap().starts_with("resp_apg_"));
        assert_eq!(openai_body["model"], "anthropic-cross");
        assert_eq!(openai_body["status"], "incomplete");
        assert_eq!(
            openai_body["output"][0]["content"][0]["text"],
            "from anthropic"
        );

        let anthropic_response = super::messages::proxy(
            Arc::clone(&state),
            principal(tenant_id),
            Utc::now(),
            "/v1/messages?client=true",
            HeaderMap::from_iter([(
                "anthropic-beta".parse().unwrap(),
                "client-beta".parse().unwrap(),
            )]),
            Bytes::from_static(
                br#"{"model":"openai-cross","messages":[{"role":"user","content":"hello"}],"max_tokens":32}"#,
            ),
            super::messages::Operation::Messages,
        )
        .await
        .unwrap();
        assert_eq!(anthropic_response.status(), StatusCode::OK);
        assert!(anthropic_response.headers().get("x-request-id").is_none());
        let anthropic_body: serde_json::Value = serde_json::from_slice(
            &to_bytes(anthropic_response.into_body(), 64 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(
            anthropic_body["id"]
                .as_str()
                .unwrap()
                .starts_with("msg_apg_")
        );
        assert_eq!(anthropic_body["model"], "openai-cross");
        assert_eq!(anthropic_body["stop_reason"], "max_tokens");
        assert_eq!(anthropic_body["content"][0]["text"], "from openai");

        {
            let seen = seen.lock().unwrap();
            let openai_target = seen.iter().find(|entry| entry.0 == "openai").unwrap();
            assert_eq!(openai_target.2["model"], "provider-openai-model");
            assert_eq!(openai_target.2["input"][0]["content"][0]["text"], "hello");
            assert!(openai_target.1.get("anthropic-beta").is_none());
            let anthropic_target = seen.iter().find(|entry| entry.0 == "anthropic").unwrap();
            assert_eq!(anthropic_target.2["model"], "provider-anthropic-model");
            assert_eq!(
                anthropic_target.2["messages"][0]["content"][0]["text"],
                "hello"
            );
            assert!(anthropic_target.1.get("openai-beta").is_none());
            assert_eq!(anthropic_target.1["anthropic-version"], "2023-06-01");
        }

        state.ledger.wait_for_idle().await;
        let ledger_statuses: serde_json::Value = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'requests', count(DISTINCT gr.id),
                'failed_requests', count(DISTINCT gr.id) FILTER (WHERE gr.status = 'failed'),
                'failed_attempts', count(pa.id) FILTER (
                    WHERE pa.status = 'failed'
                      AND pa.error_type = 'provider_response_incomplete'
                )
            )
            FROM gateway_requests gr
            JOIN provider_attempts pa
              ON (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id)
            WHERE gr.tenant_id = $1
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            ledger_statuses,
            json!({"requests": 2, "failed_requests": 2, "failed_attempts": 2})
        );
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn translated_tool_stream_keeps_protocol_order_request_echo_and_target_accounting(
        pool: PgPool,
    ) {
        let stream = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_target_tool\",\"type\":\"message\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":12,\"output_tokens\":0,\"cache_creation_input_tokens\":3}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"lookup\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let upstream = Router::new().route(
            "/v1/messages",
            post(move || async move {
                ([("content-type", "text/event-stream")], Body::from(stream)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let tenant_id = Uuid::new_v4();
        sqlx::query(
            r#"
            WITH inserted_provider AS (
                INSERT INTO providers (
                    id, tenant_id, provider_key, display_name, provider_type,
                    base_url, credential_ref, supported_operations, lifecycle_state
                ) VALUES (
                    $1, $2, 'anthropic-test', 'Anthropic stream target', 'anthropic',
                    $3, 'env://ANTHROPIC_API_KEY', ARRAY['anthropic_messages'], 'active'
                ) RETURNING id
            )
            INSERT INTO models (
                id, tenant_id, provider_id, public_model_id, provider_model_id,
                display_name, supported_operations, supports_streaming,
                supports_tools, max_output_tokens, lifecycle_state
                ) SELECT $4, $2, id, 'anthropic-stream', 'provider-anthropic-stream',
                         'Anthropic stream', ARRAY['anthropic_messages'], true, true, 4096, 'active'
              FROM inserted_provider
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(format!("http://{address}"))
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();

        let mut config = test_config();
        config.anthropic_base_url = format!("http://{address}");
        config.provider_allowed_tenant_ids = vec![tenant_id];
        let spool_dir = config.ledger_spool_dir.clone();
        let state = AppState::new(config, pool.clone()).unwrap();
        let response = openai::execute(
            &state,
            RequestContext {
                principal: principal(tenant_id),
                received_at: Utc::now(),
                path_and_query: "/v1/responses".to_owned(),
                headers: HeaderMap::new(),
                body: Bytes::from_static(
                    br#"{"model":"anthropic-stream","input":"hello","max_output_tokens":32,"stream":true,"tools":[{"type":"function","name":"lookup","parameters":{"type":"object","properties":{},"required":[],"additionalProperties":false}}],"tool_choice":"required"}"#,
                ),
                operation: OpenAiOperation::Responses,
            },
        )
        .await
        .unwrap();
        assert_eq!(response.headers["content-type"], "text/event-stream");
        let wire =
            String::from_utf8(to_bytes(response.body, 64 * 1024).await.unwrap().to_vec()).unwrap();
        assert!(wire.contains("response.function_call_arguments.delta"));
        assert!(wire.contains("response.completed"));
        assert!(wire.contains("resp_apg_"));
        assert!(wire.contains("\"max_output_tokens\":32"));
        assert!(wire.contains("\"tool_choice\":\"required\""));
        assert!(!wire.contains("msg_target_tool"));

        state.ledger.wait_for_idle().await;
        state.ledger.flush_startup().await.unwrap();
        let evidence: serde_json::Value = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'request_status', gr.status,
                'attempt_status', pa.status,
                'target_id', pa.provider_response_id,
                'input_tokens', ur.input_tokens,
                'output_tokens', ur.output_tokens,
                'cache_write_tokens', ur.cache_write_input_tokens
            )
            FROM gateway_requests gr
            JOIN provider_attempts pa
              ON (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id)
            JOIN usage_records ur
              ON (ur.tenant_id, ur.provider_attempt_id) = (pa.tenant_id, pa.id)
            WHERE gr.tenant_id = $1 AND ur.source = 'provider_reported'
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(evidence["request_status"], "completed");
        assert_eq!(evidence["attempt_status"], "completed");
        assert_eq!(evidence["target_id"], "msg_target_tool");
        assert_eq!(evidence["input_tokens"], 12);
        assert_eq!(evidence["output_tokens"], 5);
        assert_eq!(evidence["cache_write_tokens"], 3);

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn translated_openai_stream_sends_heartbeat_before_provider_headers(pool: PgPool) {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let upstream = Router::new().route("/v1/messages", post({
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            move || {
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                async move {
                    started.notify_one();
                    release.notified().await;
                    ([("content-type", "text/event-stream")], concat!(
                        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_slow\",\"type\":\"message\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
                        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
                        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                        "data: {\"type\":\"message_stop\"}\n\n"
                    ))
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let tenant_id = Uuid::new_v4();
        sqlx::query(r#"WITH p AS (INSERT INTO providers (id, tenant_id, provider_key, display_name, provider_type, base_url, credential_ref, supported_operations, lifecycle_state) VALUES ($1,$2,'anthropic-test','Anthropic','anthropic',$3,'env://ANTHROPIC_API_KEY',ARRAY['anthropic_messages'],'active') RETURNING id) INSERT INTO models (id, tenant_id, provider_id, public_model_id, provider_model_id, display_name, supported_operations, supports_streaming, max_output_tokens, lifecycle_state) SELECT $4,$2,id,'slow-model','upstream-model','Slow',ARRAY['anthropic_messages'],true,4096,'active' FROM p"#)
            .bind(Uuid::new_v4()).bind(tenant_id).bind(format!("http://{address}")).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
        let mut config = test_config();
        config.anthropic_base_url = format!("http://{address}");
        config.provider_allowed_tenant_ids = vec![tenant_id];
        config.stream_heartbeat_interval = Duration::from_millis(30);
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let task = tokio::spawn(async move {
            openai::execute(&state, RequestContext {
                principal: principal(tenant_id), received_at: Utc::now(), path_and_query: "/v1/responses".to_owned(),
                headers: HeaderMap::new(), body: Bytes::from_static(br#"{"model":"slow-model","input":"hello","max_output_tokens":16,"stream":true}"#), operation: OpenAiOperation::Responses,
            }).await
        });
        started.notified().await;
        let response = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("no translated heartbeat before provider headers")
            .unwrap()
            .unwrap();
        let mut body = response.body.into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap(), ": keepalive\n\n");
        release.notify_one();
        let mut remaining = Vec::new();
        while let Some(chunk) = body.next().await {
            remaining.extend_from_slice(&chunk.unwrap());
        }
        assert!(String::from_utf8_lossy(&remaining).contains("response.completed"));
        server.abort();
    }

    #[sqlx::test]
    async fn translated_openai_target_stream_rejects_before_provider_and_quota(pool: PgPool) {
        let stream = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_incomplete\"},\"sequence_number\":0}\n\n",
            "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_incomplete\",\"status\":\"incomplete\",\"usage\":{\"input_tokens\":9,\"output_tokens\":4,\"input_tokens_details\":{\"cache_write_tokens\":5}}},\"sequence_number\":1}\n\n"
        );
        let called = Arc::new(AtomicBool::new(false));
        let upstream_called = Arc::clone(&called);
        let upstream = Router::new().route(
            "/v1/responses",
            post(move || {
                let called = Arc::clone(&upstream_called);
                async move {
                    called.store(true, Ordering::Release);
                    ([("content-type", "text/event-stream")], Body::from(stream)).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let tenant_id = Uuid::new_v4();
        sqlx::query(
            r#"
            WITH inserted_provider AS (
                INSERT INTO providers (
                    id, tenant_id, provider_key, display_name, provider_type,
                    base_url, credential_ref, supported_operations, lifecycle_state
                ) VALUES (
                    $1, $2, 'openai-test', 'OpenAI incomplete target', 'openai',
                    $3, 'env://OPENAI_API_KEY', ARRAY['openai_responses'], 'active'
                ) RETURNING id
            ), inserted_model AS (
                INSERT INTO models (
                    id, tenant_id, provider_id, public_model_id, provider_model_id,
                    display_name, supported_operations, supports_streaming,
                    max_output_tokens, cost_metadata, lifecycle_state
                ) SELECT $4, $2, id, 'openai-incomplete', 'provider-openai-incomplete',
                         'OpenAI incomplete', ARRAY['openai_responses'], true, 4096,
                         '{"schema":"token_v1","currency":"USD","input_microusd_per_million_tokens":1000000,"output_microusd_per_million_tokens":1000000,"cache_read_input_microusd_per_million_tokens":1000000,"cache_write_input_microusd_per_million_tokens":1000000}',
                         'active'
                  FROM inserted_provider RETURNING id
            )
            INSERT INTO quotas (
                id, tenant_id, quota_key, revision, configuration, lifecycle_state
            ) SELECT $5, $2, 'cross-stream-tokens', 1,
                     '{"mode":"hard","limits":{"tokens_per_day":10000,"cost_per_day_microusd":10000}}', 'active'
              FROM inserted_model
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(format!("http://{address}"))
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();

        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        config.provider_allowed_tenant_ids = vec![tenant_id];
        let spool_dir = config.ledger_spool_dir.clone();
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
        let result = super::messages::proxy(
            Arc::clone(&state),
            principal(tenant_id),
            Utc::now(),
            "/v1/messages",
            HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"openai-incomplete","messages":[{"role":"user","content":"hello"}],"max_tokens":32,"stream":true}"#,
            ),
            super::messages::Operation::Messages,
        )
        .await;
        assert!(matches!(
            result,
            Err(crate::error::GatewayError::UnsupportedRequest)
        ));
        assert!(!called.load(Ordering::Acquire));
        let quota_counts: serde_json::Value = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'windows', (SELECT count(*) FROM quota_windows WHERE tenant_id = $1),
                'reservations', (SELECT count(*) FROM quota_reservations WHERE tenant_id = $1)
            )
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(quota_counts, json!({"windows": 0, "reservations": 0}));

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn translated_stream_rejects_a_successful_non_sse_target_response(pool: PgPool) {
        let upstream = Router::new().route(
            "/v1/messages",
            post(|| async {
                Json(json!({
                    "id": "msg_wrong_topology",
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let tenant_id = Uuid::new_v4();
        sqlx::query(
            r#"
            WITH inserted_provider AS (
                INSERT INTO providers (
                    id, tenant_id, provider_key, display_name, provider_type,
                    base_url, credential_ref, supported_operations, lifecycle_state
                ) VALUES (
                    $1, $2, 'anthropic-test', 'Anthropic target', 'anthropic',
                    $3, 'env://ANTHROPIC_API_KEY', ARRAY['anthropic_messages'], 'active'
                ) RETURNING id
            )
            INSERT INTO models (
                id, tenant_id, provider_id, public_model_id, provider_model_id,
                display_name, supported_operations, supports_streaming,
                max_output_tokens, lifecycle_state
            ) SELECT $4, $2, id, 'anthropic-non-sse', 'provider-anthropic-non-sse',
                     'Anthropic non SSE', ARRAY['anthropic_messages'], true, 4096, 'active'
              FROM inserted_provider
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(format!("http://{address}"))
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();

        let mut config = test_config();
        config.anthropic_base_url = format!("http://{address}");
        config.provider_allowed_tenant_ids = vec![tenant_id];
        let spool_dir = config.ledger_spool_dir.clone();
        let state = AppState::new(config, pool.clone()).unwrap();
        let result = openai::execute(
            &state,
            RequestContext {
                principal: principal(tenant_id),
                received_at: Utc::now(),
                path_and_query: "/v1/responses".to_owned(),
                headers: HeaderMap::new(),
                body: Bytes::from_static(
                    br#"{"model":"anthropic-non-sse","input":"hello","max_output_tokens":32,"stream":true}"#,
                ),
                operation: OpenAiOperation::Responses,
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(crate::error::GatewayError::UpstreamProtocol)
        ));
        state.ledger.wait_for_idle().await;
        state.ledger.flush_startup().await.unwrap();
        let status: String =
            sqlx::query_scalar("SELECT status FROM gateway_requests WHERE tenant_id = $1")
                .bind(tenant_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "failed");

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn model_allowlist_rejection_has_a_consistent_durable_decision(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO policies (
                id, tenant_id, policy_key, revision, configuration, lifecycle_state
            ) VALUES ($1, $2, 'global-models', 1,
                '{"constraints":{"allowed_models":["allowed-model"]}}', 'active')
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .execute(&pool)
        .await
        .unwrap();
        let mut config = test_config();
        let spool_dir = config.ledger_spool_dir.clone();
        config.provider_allowed_tenant_ids = vec![tenant_id];
        let state = AppState::new(config, pool.clone()).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-apg-task-id", "policy-task".parse().unwrap());
        let result = openai::execute(
            &state,
            RequestContext {
                principal: PrincipalContext {
                    tenant_id,
                    credential_id: Uuid::nil(),
                    credential_name: "policy-test".to_owned(),
                    user_id: None,
                    user_name: None,
                    device_id: None,
                    device_name: None,
                    client_id: None,
                    client_name: None,
                    agent_id: None,
                    agent_name: None,
                },
                received_at: Utc::now(),
                path_and_query: "/v1/chat/completions".to_owned(),
                headers,
                body: Bytes::from_static(br#"{"model":"blocked-model"}"#),
                operation: OpenAiOperation::ChatCompletions,
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(crate::error::GatewayError::ModelForbidden)
        ));
        state.ledger.wait_for_idle().await;
        state.ledger.flush_startup().await.unwrap();
        let evidence: serde_json::Value =
            sqlx::query_scalar("SELECT policy_decision FROM gateway_requests WHERE tenant_id = $1")
                .bind(tenant_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(evidence["denied"], true);
        assert_eq!(evidence["denial_reason"], "model_not_allowed");
        let task: String =
            sqlx::query_scalar("SELECT task_id FROM request_attribution WHERE tenant_id = $1")
                .bind(tenant_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(task, "policy-task");
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn a_database_outage_does_not_change_the_upstream_response_and_recovers(pool: PgPool) {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                Json(json!({
                    "id": "provider-response-during-outage",
                    "choices": [{"message": {"content": "sentinel-response"}}],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 2}
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });

        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        config.openai_provider = "test-provider".to_owned();
        let spool_dir = config.ledger_spool_dir.clone();
        let dead_pool = db::pool("postgres://gateway@127.0.0.1:1/gateway", 1).unwrap();
        let unavailable_state = Arc::new(AppState::new(config, dead_pool).unwrap());
        let response = router(unavailable_state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","messages":[{"role":"user","content":"sentinel-prompt"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response_body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let response_json: serde_json::Value = serde_json::from_slice(&response_body).unwrap();
        assert_eq!(
            response_json["choices"][0]["message"]["content"],
            "sentinel-response"
        );
        unavailable_state.ledger.wait_for_idle().await;
        assert_eq!(spool_files(&spool_dir), 1, "usage was not left durable");
        let spooled = read_spool(&spool_dir);
        assert!(!spooled.contains("sentinel-prompt"));
        assert!(!spooled.contains("sentinel-response"));

        drop(unavailable_state);
        let recovered =
            Ledger::open(pool.clone(), spool_dir.clone(), 1024 * 1024, 32 * 1024).unwrap();
        assert_eq!(recovered.flush_startup().await.unwrap(), 1);
        assert_eq!(spool_files(&spool_dir), 0);
        let counts = sqlx::query(
            r#"
            SELECT count(*) FILTER (WHERE source = 'estimated') AS estimated,
                   count(*) FILTER (WHERE source = 'provider_reported') AS provider_reported
            FROM usage_records
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(counts.get::<i64, _>("estimated"), 1);
        assert_eq!(counts.get::<i64, _>("provider_reported"), 1);
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn a_full_spool_rejects_before_calling_the_upstream() {
        let called = Arc::new(AtomicBool::new(false));
        let called_by_server = Arc::clone(&called);
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let called = Arc::clone(&called_by_server);
                async move {
                    called.store(true, Ordering::Release);
                    Json(json!({"choices": []}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });

        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        config.ledger_spool_max_bytes = 96 * 1024;
        config.ledger_spool_entry_max_bytes = 32 * 1024;
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let held = state
            .ledger
            .reserve(RequestStart {
                request_id: "held-slot".to_owned(),
                session_id: None,
                trace_id: None,
                requested_model: Some("company-default".to_owned()),
                workload_class: None,
                urgency: None,
                quality: None,
                budget_class: None,
                estimated_tokens: None,
                selection: None,
                policy: None,
                principal: PrincipalContext::bootstrap(),
                created_at: Utc::now(),
            })
            .await
            .unwrap();

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"company-default"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!called.load(Ordering::Acquire), "the upstream was called");

        held.finish(RequestCompletion {
            status: "interrupted".to_owned(),
            selected_model: None,
            selected_provider: None,
            attempts: Vec::new(),
            completed_at: Utc::now(),
        })
        .await
        .unwrap();
        state.ledger.wait_for_idle().await;
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn unclosed_terminal_sse_fragment_is_not_accepted_as_completion() {
        let upstream = Router::new().route(
            "/v1/responses",
            post(|| async {
                (
                    [("content-type", "text/event-stream")],
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_cut\"}}",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(to_bytes(response.into_body(), 4096).await.is_err());
        state.ledger.wait_for_idle().await;
        let spooled = read_spool(&spool_dir);
        assert!(spooled.contains(r#""status":"failed""#));
        assert!(spooled.contains(r#""error_type":"malformed_sse""#));
        assert!(!spooled.contains(r#""status":"completed""#));

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[derive(Clone, Debug)]
    struct SeenOpenAiRequest {
        uri: String,
        authorization: Option<String>,
        client_api_key: Option<String>,
        idempotency_key: Option<String>,
        beta: Option<String>,
        body: Vec<u8>,
    }

    #[tokio::test]
    async fn provider_secret_split_across_stream_events_is_not_reconstructed() {
        let upstream = Router::new().route("/v1/responses", post(|| async {
            ([("content-type", "text/event-stream")], "data: {\"type\":\"response.output_text.delta\",\"delta\":\"upstream-\",\"sequence_number\":1}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"key\",\"sequence_number\":2}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}},\"sequence_number\":3}\n\n")
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        let mut emitted = Vec::new();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => emitted.extend_from_slice(&bytes),
                Err(_) => break,
            }
        }
        let mut text = String::new();
        for line in String::from_utf8_lossy(&emitted).lines() {
            if let Some(data) = line.strip_prefix("data: ")
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(data)
                && let Some(delta) = value.get("delta").and_then(|value| value.as_str())
            {
                text.push_str(delta);
            }
        }
        assert!(!text.contains("upstream-key"));
        server.abort();
    }

    #[tokio::test]
    async fn escaped_secret_in_stream_metadata_is_not_sent() {
        let upstream = Router::new().route("/v1/responses", post(|| async {
            ([("content-type", "text/event-stream")], "data: {\"type\":\"response.created\",\"response\":{\"id\":\"upstream-\\u006bey\"},\"sequence_number\":0}\n\n")
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        let mut emitted = Vec::new();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => emitted.extend_from_slice(&bytes),
                Err(_) => break,
            }
        }
        assert!(!String::from_utf8_lossy(&emitted).contains("upstream-"));
        server.abort();
    }

    #[tokio::test]
    async fn streamed_provider_secret_header_is_not_sent_to_client() {
        let upstream = Router::new().route(
            "/v1/responses",
            post(|| async {
                (
                    [
                        ("content-type", "text/event-stream"),
                        ("x-request-id", "upstream-key"),
                    ],
                    "data: {}\n\n",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.headers().values().all(|value| {
            !value
                .as_bytes()
                .windows(b"upstream-key".len())
                .any(|window| window == b"upstream-key")
        }));
        server.abort();
    }

    #[tokio::test]
    async fn streamed_provider_secret_is_not_sent_to_client() {
        let upstream = Router::new().route("/v1/responses", post(|| async {
            ([("content-type", "text/event-stream")], "data: {\"type\":\"response.output_text.delta\",\"delta\":\"upstream-key\",\"sequence_number\":1}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}},\"sequence_number\":2}\n\n")
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        let mut emitted = Vec::new();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => emitted.extend_from_slice(&bytes),
                Err(_) => break,
            }
        }
        assert!(!String::from_utf8_lossy(&emitted).contains("upstream-key"));
        server.abort();
    }

    #[tokio::test]
    async fn responses_stream_is_incremental_raw_and_uses_only_the_server_secret() {
        let release = Arc::new(Notify::new());
        let release_by_server = Arc::clone(&release);
        let seen = Arc::new(Mutex::new(None));
        let seen_by_server = Arc::clone(&seen);
        let first = b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"},\"sequence_number\":0}\n\n".to_vec();
        let second = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\",\"sequence_number\":1}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":8,\"output_tokens\":2}},\"sequence_number\":2}\n\n".to_vec();
        let first_by_server = first.clone();
        let second_by_server = second.clone();
        let upstream = Router::new().route(
            "/v1/responses",
            post(
                move |OriginalUri(uri): OriginalUri, headers: HeaderMap, body: Bytes| {
                    let release = Arc::clone(&release_by_server);
                    let seen = Arc::clone(&seen_by_server);
                    let first = first_by_server.clone();
                    let second = second_by_server.clone();
                    async move {
                        *seen.lock().unwrap() = Some(SeenOpenAiRequest {
                            uri: uri.to_string(),
                            authorization: headers
                                .get("authorization")
                                .and_then(|value| value.to_str().ok())
                                .map(ToOwned::to_owned),
                            client_api_key: headers
                                .get("x-api-key")
                                .and_then(|value| value.to_str().ok())
                                .map(ToOwned::to_owned),
                            idempotency_key: headers
                                .get("idempotency-key")
                                .and_then(|value| value.to_str().ok())
                                .map(ToOwned::to_owned),
                            beta: headers
                                .get("openai-beta")
                                .and_then(|value| value.to_str().ok())
                                .map(ToOwned::to_owned),
                            body: body.to_vec(),
                        });
                        let stream = async_stream::stream! {
                            yield Ok::<_, std::io::Error>(first);
                            release.notified().await;
                            yield Ok::<_, std::io::Error>(second);
                        };
                        Response::builder()
                            .header("content-type", "text/event-stream")
                            .header("x-request-id", "openai-request-1")
                            .body(Body::from_stream(stream))
                            .unwrap()
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });

        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let request_body =
            br#"{"model":"company-default","input":"sentinel-prompt","stream":true}"#;
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses?include=one&include=two")
                    .header("authorization", "Bearer client-key")
                    .header("x-api-key", "client-key")
                    .header("idempotency-key", "client-operation-one")
                    .header("openai-beta", "responses=v1")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body.as_slice()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-request-id"], "openai-request-1");
        let mut body = response.into_body().into_data_stream();
        let first_received = tokio::time::timeout(Duration::from_secs(1), body.next())
            .await
            .expect("Responses stream buffered behind the provider barrier")
            .unwrap()
            .unwrap();
        assert_eq!(first_received.as_ref(), first);
        release.notify_one();
        let delta = body.next().await.unwrap().unwrap();
        let completed = body.next().await.unwrap().unwrap();
        assert!(body.next().await.is_none());
        assert_eq!([delta.as_ref(), completed.as_ref()].concat(), second);
        let seen = seen.lock().unwrap().clone().unwrap();
        assert_eq!(seen.uri, "/v1/responses?include=one&include=two");
        assert_eq!(seen.authorization.as_deref(), Some("Bearer upstream-key"));
        assert_eq!(seen.client_api_key, None);
        assert!(
            seen.idempotency_key
                .as_deref()
                .is_some_and(|value| value.starts_with("apg-") && value != "client-operation-one")
        );
        assert_eq!(seen.beta.as_deref(), Some("responses=v1"));
        assert_eq!(seen.body, request_body);
        state.ledger.wait_for_idle().await;
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn same_chunk_terminal_is_durable_before_the_first_event_is_released() {
        let raw = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_atomic\"}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_atomic\",\"usage\":{\"input_tokens\":9,\"output_tokens\":2}}}\n\n"
        );
        let upstream = Router::new().route(
            "/v1/responses",
            post(move || async move { ([("content-type", "text/event-stream")], raw) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        assert!(body.next().await.unwrap().is_ok());
        drop(body);
        state.ledger.wait_for_idle().await;

        let spooled = read_spool(&spool_dir);
        assert!(spooled.contains(r#""status":"completed""#));
        assert!(spooled.contains(r#""input_tokens":9"#));
        assert!(!spooled.contains(r#""status":"cancelled""#));

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn formal_response_failure_ends_cleanly_and_is_recorded_failed() {
        let raw = "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_failed\"}}\n\n";
        let upstream = Router::new().route(
            "/v1/responses",
            post(move || async move { ([("content-type", "text/event-stream")], raw) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(to_bytes(response.into_body(), 4096).await.unwrap(), raw);
        state.ledger.wait_for_idle().await;
        let spooled = read_spool(&spool_dir);
        assert!(spooled.contains(r#""status":"failed""#));
        assert!(spooled.contains(r#""error_type":"provider_response_failed""#));

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn chat_stream_preserves_finish_usage_and_done_events() {
        let raw = concat!(
            "data: {\"id\":\"chat_1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat_1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"chat_1\",\"choices\":[],\"usage\":{\"prompt_tokens\":6,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        );
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move || async move { ([("content-type", "text/event-stream")], raw) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","messages":[],"stream":true,"stream_options":{"include_usage":true}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), 64 * 1024).await.unwrap(),
            raw
        );
        state.ledger.wait_for_idle().await;
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn chat_stream_can_finish_at_eof_without_done() {
        let raw = concat!(
            "data: {\"id\":\"chat_eof\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"chat_eof\",\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\n"
        );
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move || async move { ([("content-type", "text/event-stream")], raw) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","messages":[],"stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(response.into_body(), 64 * 1024).await.unwrap(),
            raw
        );
        state.ledger.wait_for_idle().await;
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn buffered_success_never_returns_provider_secret() {
        let upstream = Router::new().route("/v1/responses", post(|| async {
            Json(json!({"id":"upstream-key","object":"response","status":"completed","model":"provider-model","output":[],"usage":{"input_tokens":1,"output_tokens":1}}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"company-default","input":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("upstream-key"));
        server.abort();
    }

    #[sqlx::test]
    async fn blocked_secret_response_is_recorded_as_failed(pool: PgPool) {
        let upstream = Router::new().route("/v1/responses", post(|| async {
            Json(json!({"id":"resp_safe","object":"response","status":"completed","model":"provider-model","output":[{"type":"message","content":[{"type":"output_text","text":"upstream-key"}]}],"usage":{"input_tokens":1,"output_tokens":1}}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("x-request-id", "blocked-secret-ledger")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"company-default","input":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        state.ledger.wait_for_idle().await;
        state.ledger.flush_startup().await.unwrap();
        let row = sqlx::query("SELECT gr.status AS request_status, pa.status AS attempt_status, pa.error_type FROM gateway_requests gr JOIN provider_attempts pa ON (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id) WHERE gr.request_id = 'blocked-secret-ledger'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(row.get::<String, _>("request_status"), "failed");
        assert_eq!(row.get::<String, _>("attempt_status"), "failed");
        assert_eq!(
            row.get::<String, _>("error_type"),
            "provider_secret_in_response"
        );
        server.abort();
    }

    #[sqlx::test]
    async fn blocked_anthropic_secret_response_is_recorded_as_failed(pool: PgPool) {
        let upstream = Router::new().route("/v1/messages", post(|| async {
            Json(json!({"id":"msg_safe","type":"message","role":"assistant","content":[{"type":"text","text":"provider-key"}],"model":"claude-test","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.anthropic_base_url = format!("http://{address}");
        config.allowed_models = vec!["claude-test".to_owned()];
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", "client-key")
                    .header("x-request-id", "blocked-anthropic-secret-ledger")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"claude-test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        state.ledger.wait_for_idle().await;
        state.ledger.flush_startup().await.unwrap();
        let row = sqlx::query("SELECT gr.status AS request_status, pa.status AS attempt_status, pa.error_type FROM gateway_requests gr JOIN provider_attempts pa ON (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id) WHERE gr.request_id = 'blocked-anthropic-secret-ledger'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(row.get::<String, _>("request_status"), "failed");
        assert_eq!(row.get::<String, _>("attempt_status"), "failed");
        assert_eq!(
            row.get::<String, _>("error_type"),
            "provider_secret_in_response"
        );
        server.abort();
    }

    #[tokio::test]
    async fn upstream_error_never_returns_provider_secret() {
        let upstream = Router::new().route(
            "/v1/responses",
            post(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":{"message":"bad upstream-key"}})),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"company-default","input":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("upstream-key"));
        server.abort();
    }

    #[tokio::test]
    async fn openai_upstream_rejection_preserves_status_code_body_and_retry_header() {
        let raw = Bytes::from_static(
            br#"{ "error": {"code":"rate_limit_exceeded","message":"try later","type":"rate_limit_error"}, "vendor_number":123456789012345678901234567890 }"#,
        );
        let raw_by_server = raw.clone();
        let upstream = Router::new().route(
            "/v1/responses",
            post(move || {
                let raw = raw_by_server.clone();
                async move {
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("content-type", "application/json"), ("retry-after", "11")],
                        raw,
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"company-default","input":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "11");
        assert_eq!(to_bytes(response.into_body(), 4096).await.unwrap(), raw);

        state.ledger.wait_for_idle().await;
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn background_responses_are_rejected_before_the_provider_call() {
        let called = Arc::new(AtomicBool::new(false));
        let called_by_server = Arc::clone(&called);
        let upstream = Router::new().route(
            "/v1/responses",
            post(move || {
                called_by_server.store(true, Ordering::Release);
                async { Json(json!({"id": "must-not-run", "status": "completed"})) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());

        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","background":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "unsupported_request");
        assert!(!called.load(Ordering::Acquire));

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn buffered_failed_response_is_not_accounted_as_completed() {
        let upstream = Router::new().route(
            "/v1/responses",
            post(|| async {
                Json(json!({
                    "id": "resp_failed",
                    "status": "failed",
                    "error": {"code": "provider_failed"},
                    "usage": {
                        "input_tokens": 8,
                        "input_tokens_details": {"cached_tokens": 3, "cache_write_tokens": 2},
                        "output_tokens": 1
                    }
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"company-default","input":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        state.ledger.wait_for_idle().await;
        let spooled = read_spool(&spool_dir);
        assert!(spooled.contains(r#""status":"failed""#));
        assert!(spooled.contains(r#""error_type":"provider_response_failed""#));
        assert!(spooled.contains(r#""cached_input_tokens":3"#));
        assert!(spooled.contains(r#""cache_write_input_tokens":2"#));
        assert!(!spooled.contains(r#""status":"completed""#));

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn non_stream_transport_failure_is_recorded_as_failed_not_cancelled() {
        let upstream = Router::new().route(
            "/v1/responses",
            post(|| async {
                let body = async_stream::stream! {
                    yield Ok::<_, std::io::Error>(Bytes::from_static(b"{\"id\":\"partial"));
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    yield Err(std::io::Error::other("provider cut the response body"));
                };
                Response::builder()
                    .header("content-type", "application/json")
                    .body(Body::from_stream(body))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"company-default","input":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        state.ledger.wait_for_idle().await;
        let spooled = read_spool(&spool_dir);
        assert!(spooled.contains(r#""status":"failed""#));
        assert!(spooled.contains(r#""error_type":"upstream_body_error""#));
        assert!(!spooled.contains(r#""error_type":"cancelled""#));

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn malformed_openai_sse_fails_the_body_and_is_logged() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_by_server = Arc::clone(&calls);
        let upstream = Router::new().route(
            "/v1/responses",
            post(move || {
                let calls = Arc::clone(&calls_by_server);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    (
                        [("content-type", "text/event-stream")],
                        "{\"type\":\"response.completed\"}\n\n",
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let logs = logging::capture::Buffer::default();
        let (subscriber, _provider) = logging::test_subscriber(logs.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(to_bytes(response.into_body(), 4096).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(logs.lines().iter().any(|line| {
            line["event"]["name"] == "http.access"
                && line["error"]["type"] == "stream_body_error"
                && line["exception"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("missing data"))
        }));
        state.ledger.wait_for_idle().await;
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[derive(Clone, Debug)]
    struct SeenAnthropicRequest {
        uri: String,
        authorization: Option<String>,
        api_key: Option<String>,
        version: Option<String>,
        beta: Option<String>,
        body: Vec<u8>,
    }

    fn gated_anthropic_stream(
        first: Vec<u8>,
        second: Vec<u8>,
        release: tokio::sync::oneshot::Receiver<()>,
        tail_sent: Arc<AtomicUsize>,
    ) -> impl futures_util::Stream<Item = Result<Vec<u8>, std::io::Error>> + Send {
        async_stream::stream! {
            yield Ok(first);
            release.await.unwrap();
            tail_sent.store(1, Ordering::SeqCst);
            yield Ok(second);
            std::future::pending::<()>().await;
        }
    }

    #[tokio::test]
    async fn anthropic_stream_fixture_rejects_collecting_before_headers() {
        let (release, receiver) = tokio::sync::oneshot::channel();
        let tail_sent = Arc::new(AtomicUsize::new(0));
        let body = Body::from_stream(gated_anthropic_stream(
            b"data: first\n\n".to_vec(),
            b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_vec(),
            receiver,
            Arc::clone(&tail_sent),
        ));
        let buffered_response = async {
            let collected = to_bytes(body, 4096).await.unwrap();
            Response::new(Body::from(collected))
        };
        tokio::pin!(buffered_response);
        assert!(futures_util::poll!(&mut buffered_response).is_pending());
        assert_eq!(tail_sent.load(Ordering::SeqCst), 0);
        release.send(()).unwrap();
        assert!(futures_util::poll!(&mut buffered_response).is_pending());
        assert_eq!(tail_sent.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn anthropic_stream_is_incremental_and_preserves_the_wire_contract() {
        tokio::time::timeout(
            Duration::from_secs(30),
            exercise_incremental_anthropic_stream(),
        )
        .await
        .expect("the stream contract test did not finish; inspect the blocked phase");
    }

    async fn exercise_incremental_anthropic_stream() {
        let log_buffer = logging::capture::Buffer::default();
        let (subscriber, _traces) = logging::test_subscriber(log_buffer.clone());
        let log_guard = tracing::subscriber::set_default(subscriber);
        let (release, release_receiver) = tokio::sync::oneshot::channel::<()>();
        let release_for_server = Arc::new(Mutex::new(Some(release_receiver)));
        let tail_sent = Arc::new(AtomicUsize::new(0));
        let tail_sent_by_server = Arc::clone(&tail_sent);
        let seen = Arc::new(Mutex::new(None));
        let seen_by_server = Arc::clone(&seen);
        let first = b": keepalive\n\nevent: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":7}}}\n\n".to_vec();
        let second = b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_vec();
        let first_by_server = first.clone();
        let second_by_server = second.clone();
        let upstream = Router::new().route(
            "/v1/messages",
            post(
                move |OriginalUri(uri): OriginalUri,
                      headers: axum::http::HeaderMap,
                      body: axum::body::Bytes| {
                    let release = release_for_server.lock().unwrap().take().unwrap();
                    let tail_sent = Arc::clone(&tail_sent_by_server);
                    let seen = Arc::clone(&seen_by_server);
                    let first = first_by_server.clone();
                    let second = second_by_server.clone();
                    async move {
                        *seen.lock().unwrap() = Some(SeenAnthropicRequest {
                            uri: uri.to_string(),
                            authorization: headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .map(ToOwned::to_owned),
                            api_key: headers
                                .get("x-api-key")
                                .and_then(|v| v.to_str().ok())
                                .map(ToOwned::to_owned),
                            version: headers
                                .get("anthropic-version")
                                .and_then(|v| v.to_str().ok())
                                .map(ToOwned::to_owned),
                            beta: headers
                                .get("anthropic-beta")
                                .and_then(|v| v.to_str().ok())
                                .map(ToOwned::to_owned),
                            body: body.to_vec(),
                        });
                        let stream = gated_anthropic_stream(first, second, release, tail_sent);
                        Response::builder()
                            .header("content-type", "text/event-stream")
                            .header("request-id", "provider-request-1")
                            .body(Body::from_stream(stream))
                            .unwrap()
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

        let mut config = test_config();
        config.anthropic_base_url = format!("http://{address}");
        config.allowed_models = vec!["claude-test".to_owned()];
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let request_body = br#"{"model":"claude-test","stream":true,"system":[{"type":"text","text":"rules","cache_control":{"type":"ephemeral"}}]}"#;
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages?beta=true&x=a%2Fb&x=2")
                    .header("x-api-key", "client-key")
                    .header("anthropic-version", "2023-06-01")
                    .header("anthropic-beta", "feature-a,feature-b")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body.as_slice()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(response.headers()["request-id"], "provider-request-1");

        let mut response_body = response.into_body().into_data_stream();
        let keepalive_received = response_body.next().await.unwrap().unwrap();
        let start_received = response_body.next().await.unwrap().unwrap();
        assert_eq!(
            [keepalive_received.as_ref(), start_received.as_ref()].concat(),
            first
        );
        assert!(
            !log_buffer
                .lines()
                .iter()
                .any(|line| line["event"]["name"] == "http.access"),
            "the request was logged complete while its body was still active"
        );
        assert_eq!(tail_sent.load(Ordering::SeqCst), 0);
        release.send(()).unwrap();
        let delta_received = response_body.next().await.unwrap().unwrap();
        let terminal_received = response_body.next().await.unwrap().unwrap();
        let ended = response_body.next().await;
        assert_eq!(tail_sent.load(Ordering::SeqCst), 1);
        assert!(ended.is_none());
        let relayed = [
            keepalive_received.as_ref(),
            start_received.as_ref(),
            delta_received.as_ref(),
            terminal_received.as_ref(),
        ]
        .concat();
        assert_eq!(relayed, [first, second].concat());
        assert_eq!(
            log_buffer
                .lines()
                .iter()
                .filter(|line| line["event"]["name"] == "http.access")
                .count(),
            1,
            "stream completion did not produce exactly one access line"
        );

        let observed = seen.lock().unwrap().clone().unwrap();
        assert_eq!(observed.uri, "/v1/messages?beta=true&x=a%2Fb&x=2");
        assert_eq!(observed.authorization, None);
        assert_eq!(observed.api_key.as_deref(), Some("provider-key"));
        assert_eq!(observed.version.as_deref(), Some("2023-06-01"));
        assert_eq!(observed.beta.as_deref(), Some("feature-a,feature-b"));
        assert_eq!(observed.body, request_body);

        state.ledger.wait_for_idle().await;
        drop(log_guard);
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn anthropic_stream_sends_heartbeat_before_delayed_provider_headers() {
        tokio::time::timeout(
            Duration::from_secs(30),
            exercise_heartbeat_before_provider_headers(),
        )
        .await
        .expect("the heartbeat contract test did not finish; inspect the blocked phase");
    }

    #[tokio::test]
    async fn openai_stream_sends_heartbeat_before_delayed_provider_headers() {
        let release = Arc::new(Notify::new());
        let started = Arc::new(Notify::new());
        let upstream = Router::new().route("/v1/responses", post({
            let release = Arc::clone(&release);
            let started = Arc::clone(&started);
            move || {
                let release = Arc::clone(&release);
                let started = Arc::clone(&started);
                async move {
                    started.notify_one();
                    release.notified().await;
                    ([("content-type", "text/event-stream")], concat!(
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_slow\"},\"sequence_number\":0}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_slow\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}},\"sequence_number\":1}\n\n"
                    ))
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        config.stream_heartbeat_interval = Duration::from_millis(30);
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let pending = tokio::spawn(
            router(state).oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":true}"#,
                    ))
                    .unwrap(),
            ),
        );
        started.notified().await;
        let response = tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .expect("no heartbeat before provider headers")
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap(), ": keepalive\n\n");
        release.notify_one();
        let mut remaining = Vec::new();
        while let Some(chunk) = body.next().await {
            remaining.extend_from_slice(&chunk.unwrap());
        }
        assert!(String::from_utf8_lossy(&remaining).contains("response.completed"));
        server.abort();
    }

    #[sqlx::test]
    async fn delayed_openai_stream_error_does_not_leak_the_provider_key(pool: PgPool) {
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = Arc::clone(&calls);
        let upstream = Router::new().route(
            "/v1/responses",
            post(move || {
                let calls = Arc::clone(&server_calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    (StatusCode::TOO_MANY_REQUESTS, "upstream-key")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        config.stream_heartbeat_interval = Duration::from_millis(20);
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .header("x-request-id", "delayed-stream-error")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains(": keepalive") && text.contains("\"type\":\"error\""));
        assert!(!text.contains("upstream-key"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        state.ledger.wait_for_idle().await;
        state.ledger.flush_startup().await.unwrap();
        let row = sqlx::query("SELECT gr.status AS request_status, pa.status AS attempt_status, pa.error_type FROM gateway_requests gr JOIN provider_attempts pa ON (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id) WHERE gr.request_id = 'delayed-stream-error'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(row.get::<String, _>("request_status"), "failed");
        assert_eq!(row.get::<String, _>("attempt_status"), "failed");
        assert_eq!(row.get::<String, _>("error_type"), "upstream_status_429");
        server.abort();
    }

    #[tokio::test]
    async fn dropping_openai_stream_while_waiting_closes_provider_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let closed = Arc::new(Notify::new());
        let server_closed = Arc::clone(&closed);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0u8; 4096];
            loop {
                match socket.read(&mut bytes).await {
                    Ok(0) | Err(_) => {
                        server_closed.notify_one();
                        break;
                    }
                    Ok(_) => {}
                }
            }
        });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        config.stream_heartbeat_interval = Duration::from_millis(20);
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap(), ": keepalive\n\n");
        drop(body);
        tokio::time::timeout(Duration::from_secs(2), closed.notified())
            .await
            .expect("provider connection remained open");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn delayed_chat_stream_error_uses_chat_error_format() {
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = Arc::clone(&calls);
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let calls = Arc::clone(&server_calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    (StatusCode::TOO_MANY_REQUESTS, "upstream-key")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        config.stream_heartbeat_interval = Duration::from_millis(20);
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","messages":[],"stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let event = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(event).unwrap();
        assert_eq!(value["error"]["type"], "api_error");
        assert!(!text.contains("upstream-key"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }

    async fn exercise_heartbeat_before_provider_headers() {
        let release = Arc::new(Notify::new());
        let release_for_server = Arc::clone(&release);
        let started = Arc::new(Notify::new());
        let started_by_server = Arc::clone(&started);
        let upstream = Router::new().route(
            "/v1/messages",
            post(move || {
                let started = Arc::clone(&started_by_server);
                let release = Arc::clone(&release_for_server);
                async move {
                    started.notify_one();
                    release.notified().await;
                    (
                        [("content-type", "text/event-stream")],
                        concat!(
                            "event: message_start\n",
                            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_slow\"}}\n\n",
                            "event: message_stop\n",
                            "data: {\"type\":\"message_stop\"}\n\n"
                        ),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.anthropic_base_url = format!("http://{address}");
        config.allowed_models = vec!["claude-test".to_owned()];
        config.stream_heartbeat_interval = Duration::from_millis(30);
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());

        let request = tokio::spawn({
            let state = Arc::clone(&state);
            async move {
                router(state)
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/v1/messages")
                            .header("x-api-key", "client-key")
                            .header("content-type", "application/json")
                            .body(Body::from(r#"{"model":"claude-test","stream":true}"#))
                            .unwrap(),
                    )
                    .await
            }
        });
        started.notified().await;
        let response = request.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        let heartbeat = body.next().await.unwrap().unwrap();
        assert_eq!(heartbeat, ": keepalive\n\n");
        release.notify_one();
        let mut remainder = Vec::new();
        while let Some(chunk) = body.next().await {
            remainder.extend_from_slice(&chunk.unwrap());
        }
        assert!(
            String::from_utf8(remainder)
                .unwrap()
                .contains("message_stop")
        );

        state.ledger.wait_for_idle().await;
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn eof_without_message_stop_is_a_stream_error() {
        let upstream = Router::new().route(
            "/v1/messages",
            post(|| async {
                (
                    [("content-type", "text/event-stream")],
                    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_partial\"}}\n\n",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.anthropic_base_url = format!("http://{address}");
        config.allowed_models = vec!["claude-test".to_owned()];
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let logs = logging::capture::Buffer::default();
        let (subscriber, _provider) = logging::test_subscriber(logs.clone());
        let _guard = tracing::subscriber::set_default(subscriber);

        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", "client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"claude-test","stream":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(to_bytes(response.into_body(), 4096).await.is_err());
        assert!(logs.lines().iter().any(|line| {
            line["event"]["name"] == "http.access" && line["error"]["type"] == "stream_body_error"
        }));

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    struct DisconnectSignal(Arc<Notify>);

    impl Drop for DisconnectSignal {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    #[tokio::test]
    async fn dropping_the_client_body_closes_the_provider_stream() {
        let disconnected = Arc::new(Notify::new());
        let disconnected_by_server = Arc::clone(&disconnected);
        let upstream = Router::new().route(
            "/v1/messages",
            post(move || {
                let signal = Arc::clone(&disconnected_by_server);
                async move {
                    let stream = async_stream::stream! {
                        let _signal = DisconnectSignal(signal);
                        yield Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
                            b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_cancel\",\"usage\":{\"input_tokens\":1}}}\n\n",
                        ));
                        std::future::pending::<()>().await;
                    };
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(Body::from_stream(stream))
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

        let mut config = test_config();
        config.anthropic_base_url = format!("http://{address}");
        config.allowed_models = vec!["claude-test".to_owned()];
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let ledger = state.ledger.clone();
        let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_address = gateway_listener.local_addr().unwrap();
        let gateway_task =
            tokio::spawn(
                async move { axum::serve(gateway_listener, router(state)).await.unwrap() },
            );
        let response = reqwest::Client::new()
            .post(format!("http://{gateway_address}/v1/messages"))
            .header("x-api-key", "client-key")
            .header("content-type", "application/json")
            .body(r#"{"model":"claude-test","stream":true}"#)
            .send()
            .await
            .unwrap();
        let mut response_body = response.bytes_stream();
        assert!(response_body.next().await.unwrap().is_ok());
        drop(response_body);
        tokio::time::timeout(Duration::from_secs(1), disconnected.notified())
            .await
            .expect("the provider connection remained open after client cancellation");

        upstream_task.abort();
        gateway_task.abort();
        wait_for_finalized_spool(&ledger, &spool_dir).await;
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn dropping_an_openai_client_closes_the_provider_stream() {
        let disconnected = Arc::new(Notify::new());
        let disconnected_by_server = Arc::clone(&disconnected);
        let upstream = Router::new().route(
            "/v1/responses",
            post(move || {
                let signal = Arc::clone(&disconnected_by_server);
                async move {
                    let stream = async_stream::stream! {
                        let _signal = DisconnectSignal(signal);
                        yield Ok::<_, std::io::Error>(Bytes::from_static(
                            b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_cancel\"}}\n\n",
                        ));
                        std::future::pending::<()>().await;
                    };
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(Body::from_stream(stream))
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let ledger = state.ledger.clone();
        let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_address = gateway_listener.local_addr().unwrap();
        let gateway_task =
            tokio::spawn(
                async move { axum::serve(gateway_listener, router(state)).await.unwrap() },
            );
        let response = reqwest::Client::new()
            .post(format!("http://{gateway_address}/v1/responses"))
            .header("authorization", "Bearer client-key")
            .header("content-type", "application/json")
            .body(r#"{"model":"company-default","input":"x","stream":true}"#)
            .send()
            .await
            .unwrap();
        let mut response_body = response.bytes_stream();
        assert!(response_body.next().await.unwrap().is_ok());
        drop(response_body);
        tokio::time::timeout(Duration::from_secs(1), disconnected.notified())
            .await
            .expect("the OpenAI provider connection remained open after client cancellation");

        upstream_task.abort();
        gateway_task.abort();
        wait_for_finalized_spool(&ledger, &spool_dir).await;
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn anthropic_stream_persists_ids_timings_and_separate_usage(pool: PgPool) {
        let upstream = Router::new()
            .route(
                "/v1/messages",
                post(|headers: HeaderMap| async move {
                    assert!(!headers.contains_key("x-apg-task-id"));
                    (
                        [("content-type", "text/event-stream"), ("request-id", "provider-req-db")],
                        concat!(
                            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_db\",\"usage\":{\"input_tokens\":13,\"cache_read_input_tokens\":5,\"cache_creation_input_tokens\":2}}}\n\n",
                            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":4}}\n\n",
                            "data: {\"type\":\"message_stop\"}\n\n"
                        ),
                    )
                }),
            )
            .route(
                "/v1/messages/count_tokens",
                post(|| async { Json(json!({"input_tokens": 99})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

        let mut config = test_config();
        config.database_url = "managed-by-sqlx-test".to_owned();
        config.anthropic_base_url = format!("http://{address}");
        config.allowed_models = vec!["claude-test".to_owned()];
        let spool_dir = config.ledger_spool_dir.clone();
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
        let buffer = logging::capture::Buffer::default();
        let (subscriber, _provider) = logging::test_subscriber(buffer);
        let guard = tracing::subscriber::set_default(subscriber);

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", "client-key")
                    .header("x-request-id", "gateway-wire-id")
                    .header("x-apg-task-id", "anthropic-task")
                    .header("x-claude-code-session-id", "session-1")
                    .header("x-claude-code-agent-id", "agent-1")
                    .header("x-claude-code-parent-agent-id", "parent-1")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"claude-test","stream":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("message_stop"));
        state.ledger.wait_for_idle().await;

        let row = sqlx::query(
            r#"
            SELECT gr.id, gr.session_id, gr.trace_id, gr.client_agent_id,
                   gr.parent_agent_id, gr.request_received_at, gr.queue_entered_at,
                   gr.queue_left_at, gr.client_first_byte_at, gr.completed_at,
                   gr.selected_provider,
                   gr.total_latency IS NOT NULL AS total_latency_present,
                   gr.gateway_overhead IS NOT NULL AS gateway_overhead_present,
                   pa.id AS attempt_id, pa.provider, pa.provider_request_id,
                   pa.provider_response_id, pa.provider_request_at,
                   pa.provider_first_byte_at,
                   pa.provider_ttft IS NOT NULL AS provider_ttft_present,
                   (SELECT task_id FROM request_attribution a
                    WHERE (a.tenant_id, a.gateway_request_id) = (gr.tenant_id, gr.id)) AS task_id
            FROM gateway_requests gr
            JOIN provider_attempts pa ON (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id)
            WHERE gr.request_id = 'gateway-wire-id'
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!row.get::<Uuid, _>("id").is_nil());
        assert!(!row.get::<Uuid, _>("attempt_id").is_nil());
        assert_eq!(row.get::<String, _>("session_id"), "session-1");
        assert_eq!(row.get::<String, _>("task_id"), "anthropic-task");
        assert_eq!(row.get::<String, _>("client_agent_id"), "agent-1");
        assert_eq!(row.get::<String, _>("parent_agent_id"), "parent-1");
        assert_eq!(
            row.get::<String, _>("provider_request_id"),
            "provider-req-db"
        );
        assert_eq!(row.get::<String, _>("provider_response_id"), "msg_db");
        assert_eq!(row.get::<String, _>("selected_provider"), "anthropic-test");
        assert_eq!(row.get::<String, _>("provider"), "anthropic-test");
        assert_eq!(row.get::<String, _>("trace_id").len(), 32);
        for field in [
            "request_received_at",
            "client_first_byte_at",
            "completed_at",
            "provider_request_at",
            "provider_first_byte_at",
        ] {
            assert!(
                row.try_get::<chrono::DateTime<Utc>, _>(field).is_ok(),
                "missing {field}"
            );
        }
        assert!(row.get::<bool, _>("total_latency_present"));
        assert!(row.get::<bool, _>("gateway_overhead_present"));
        assert!(row.get::<bool, _>("provider_ttft_present"));
        assert!(
            row.try_get::<chrono::DateTime<Utc>, _>("queue_entered_at")
                .is_err()
        );
        assert!(
            row.try_get::<chrono::DateTime<Utc>, _>("queue_left_at")
                .is_err()
        );

        let usage = sqlx::query(
            "SELECT source, input_tokens, output_tokens, cached_input_tokens, cache_write_input_tokens FROM usage_records ORDER BY source",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(usage.len(), 2);
        assert_eq!(usage[0].get::<String, _>("source"), "estimated");
        assert_eq!(usage[1].get::<String, _>("source"), "provider_reported");
        assert_eq!(usage[1].get::<i64, _>("input_tokens"), 13);
        assert_eq!(usage[1].get::<i64, _>("output_tokens"), 4);
        assert_eq!(usage[1].get::<i64, _>("cached_input_tokens"), 5);
        assert_eq!(usage[1].get::<i64, _>("cache_write_input_tokens"), 2);

        let count_response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages/count_tokens")
                    .header("authorization", "Bearer client-key")
                    .header("x-request-id", "count-token-id")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"claude-test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(count_response.status(), StatusCode::OK);
        let _ = to_bytes(count_response.into_body(), 1024).await.unwrap();
        state.ledger.wait_for_idle().await;
        let count_usage = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM usage_records ur JOIN gateway_requests gr ON gr.id = ur.gateway_request_id WHERE gr.request_id = 'count-token-id'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count_usage, 0);

        drop(guard);
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn responses_stream_persists_provider_usage_and_ids(pool: PgPool) {
        let upstream = Router::new().route(
            "/v1/responses",
            post(|| async {
                (
                    [
                        ("content-type", "text/event-stream"),
                        ("x-request-id", "provider-request-responses"),
                    ],
                    concat!(
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_db\"},\"sequence_number\":0}\n\n",
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\",\"sequence_number\":1}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_db\",\"usage\":{\"input_tokens\":12,\"input_tokens_details\":{\"cached_tokens\":4},\"output_tokens\":5,\"output_tokens_details\":{\"reasoning_tokens\":2}}},\"sequence_number\":2}\n\n"
                    ),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        config.openai_provider = "openai-test-provider".to_owned();
        let spool_dir = config.ledger_spool_dir.clone();
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("x-request-id", "responses-ledger-id")
                    .header("x-codex-session-id", "codex-session-one")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("response.completed"));
        state.ledger.flush_startup().await.unwrap();
        let row = sqlx::query(
            r#"
            SELECT gr.request_id, gr.session_id, gr.operation, gr.status,
                   gr.selected_provider, pa.provider, pa.provider_request_id,
                   pa.provider_response_id,
                   ur.input_tokens, ur.output_tokens, ur.cached_input_tokens,
                   ur.reasoning_tokens
            FROM gateway_requests gr
            JOIN provider_attempts pa
              ON (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id)
            JOIN usage_records ur
              ON (ur.tenant_id, ur.gateway_request_id, ur.provider_attempt_id) =
                 (pa.tenant_id, pa.gateway_request_id, pa.id)
            WHERE gr.request_id = 'responses-ledger-id'
              AND ur.source = 'provider_reported'
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("session_id"), "codex-session-one");
        assert_eq!(row.get::<String, _>("operation"), "responses");
        assert_eq!(row.get::<String, _>("status"), "completed");
        assert_eq!(
            row.get::<String, _>("selected_provider"),
            "openai-test-provider"
        );
        assert_eq!(row.get::<String, _>("provider"), "openai-test-provider");
        assert_eq!(
            row.get::<String, _>("provider_request_id"),
            "provider-request-responses"
        );
        assert_eq!(row.get::<String, _>("provider_response_id"), "resp_db");
        assert_eq!(row.get::<i64, _>("input_tokens"), 8);
        assert_eq!(row.get::<i64, _>("output_tokens"), 3);
        assert_eq!(row.get::<i64, _>("cached_input_tokens"), 4);
        assert_eq!(row.get::<i64, _>("reasoning_tokens"), 2);

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[sqlx::test]
    async fn malformed_required_usage_is_durably_finalized_as_failed(pool: PgPool) {
        let upstream = Router::new().route(
            "/v1/responses",
            post(|| async {
                (
                    [("content-type", "application/json")],
                    r#"{"id":"resp_bad_usage","status":"completed","output":[],"usage":"invalid"}"#,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.openai_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer client-key")
                    .header("x-request-id", "bad-usage-ledger-id")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"company-default","input":"x","stream":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        state.ledger.flush_startup().await.unwrap();
        let row = sqlx::query(
            r#"
            SELECT gr.status, pa.status AS attempt_status, pa.error_type
            FROM gateway_requests gr
            JOIN provider_attempts pa
              ON (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id)
            WHERE gr.request_id = 'bad-usage-ledger-id'
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("status"), "failed");
        assert_eq!(row.get::<String, _>("attempt_status"), "failed");
        assert_eq!(
            row.get::<String, _>("error_type"),
            "upstream_protocol_error"
        );

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn claude_discovery_prewarm_auth_and_error_contracts_hold() {
        let encoded_error = axum::body::Bytes::from_static(b"\x1f\x8braw-gzip-sentinel");
        let encoded_by_server = encoded_error.clone();
        let upstream = Router::new().route(
            "/v1/messages",
            post(move || {
                let encoded = encoded_by_server.clone();
                async move {
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [
                            ("content-type", "application/json"),
                            ("content-encoding", "gzip"),
                            ("retry-after", "7"),
                        ],
                        encoded,
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut config = test_config();
        config.anthropic_base_url = format!("http://{address}");
        config.allowed_models = vec!["claude-test".to_owned(), "anthropic-fast".to_owned()];
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());

        let models = tokio::time::timeout(
            Duration::from_secs(3),
            router(state.clone()).oneshot(
                Request::builder()
                    .uri("/v1/models?limit=1000")
                    .header("x-api-key", "client-key")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect("model discovery exceeded three seconds")
        .unwrap();
        assert_eq!(models.status(), StatusCode::OK);
        assert!(models.headers().get("location").is_none());
        let models: serde_json::Value =
            serde_json::from_slice(&to_bytes(models.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(models["data"][0]["id"], "claude-test");
        assert_eq!(models["data"][0]["stream_idle_timeout_ms"], 600_000);
        assert_eq!(models["data"][0]["stream_max_retries"], 0);

        let hello = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri("/api/hello")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(hello.status(), StatusCode::NO_CONTENT);

        let conflict = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("authorization", "Bearer client-key")
                    .header("x-api-key", "different-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"claude-test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::UNAUTHORIZED);

        let rejected = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", "client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"claude-test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rejected.headers()["retry-after"], "7");
        assert_eq!(rejected.headers()["content-encoding"], "gzip");
        assert_eq!(
            to_bytes(rejected.into_body(), 4096).await.unwrap().as_ref(),
            encoded_error.as_ref()
        );

        state.ledger.wait_for_idle().await;
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn app_header_limit_returns_413_over_raw_tcp() {
        let mut config = test_config();
        config.request_header_max_bytes = 8 * 1024;
        config.shutdown_finalize_timeout = Duration::from_millis(100);
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel();
        let gateway_task =
            tokio::spawn(startup::serve_with_shutdown(listener, state, async move {
                let _ = signal_rx.await;
            }));

        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "GET /health HTTP/1.1\r\nHost: gateway\r\nX-Fill: {}\r\nConnection: close\r\n\r\n",
            "a".repeat(9 * 1024)
        );
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert!(
            String::from_utf8_lossy(&response).starts_with("HTTP/1.1 413"),
            "the application header limit must reject with 413"
        );

        signal_tx.send(()).unwrap();
        gateway_task.await.unwrap().unwrap();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn transport_header_limit_returns_431_over_raw_tcp() {
        let mut config = test_config();
        config.request_header_max_bytes = 8 * 1024;
        config.shutdown_finalize_timeout = Duration::from_millis(100);
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel();
        let gateway_task =
            tokio::spawn(startup::serve_with_shutdown(listener, state, async move {
                let _ = signal_rx.await;
            }));

        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "GET /health HTTP/1.1\r\nHost: gateway\r\nX-Fill: {}\r\nConnection: close\r\n\r\n",
            "a".repeat(17 * 1024)
        );
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert!(
            String::from_utf8_lossy(&response).starts_with("HTTP/1.1 431"),
            "the transport parser must reject oversized headers with 431"
        );

        signal_tx.send(()).unwrap();
        gateway_task.await.unwrap().unwrap();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn drain_keeps_health_reachable_and_existing_stream_running() {
        let release = Arc::new(Notify::new());
        let release_for_provider = Arc::clone(&release);
        let first = Bytes::from_static(
            b": keepalive\n\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_drain\",\"usage\":{\"input_tokens\":7}}}\n\n",
        );
        let second = Bytes::from_static(
            b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let expected_second = second.clone();
        let upstream = Router::new().route(
            "/v1/messages",
            post(move || {
                let release = Arc::clone(&release_for_provider);
                let first = first.clone();
                let second = second.clone();
                async move {
                    let stream = async_stream::stream! {
                        yield Ok::<_, std::io::Error>(first);
                        release.notified().await;
                        yield Ok::<_, std::io::Error>(second);
                    };
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .header("request-id", "provider-drain")
                        .body(Body::from_stream(stream))
                        .unwrap()
                }
            }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_address = upstream_listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(upstream_listener, upstream).await.unwrap() });

        let mut config = test_config();
        config.anthropic_base_url = format!("http://{upstream_address}");
        config.allowed_models = vec!["claude-test".to_owned()];
        config.drain_timeout = Duration::from_secs(2);
        config.shutdown_finalize_timeout = Duration::from_millis(100);
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let lifecycle = state.lifecycle.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel();
        let gateway_state = Arc::clone(&state);
        let gateway_task = tokio::spawn(async move {
            startup::serve_with_shutdown(listener, gateway_state, async move {
                let _ = signal_rx.await;
            })
            .await
            .unwrap()
        });

        let response = reqwest::Client::new()
            .post(format!("http://{address}/v1/messages"))
            .header("x-api-key", "client-key")
            .header("content-type", "application/json")
            .body(r#"{"model":"claude-test","stream":true}"#)
            .send()
            .await
            .unwrap();
        let mut body = response.bytes_stream();
        let first_received = body.next().await.unwrap().unwrap();
        signal_tx.send(()).unwrap();
        lifecycle.wait_for_draining().await;

        let mut probe = tokio::net::TcpStream::connect(address).await.unwrap();
        probe
            .write_all(
                b"GET /ready HTTP/1.1\r\nHost: gateway\r\n\r\nGET /health HTTP/1.1\r\nHost: gateway\r\n\r\nGET /v1/models HTTP/1.1\r\nHost: gateway\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut probes = Vec::new();
        probe.read_to_end(&mut probes).await.unwrap();
        let probes = String::from_utf8(probes).unwrap();
        assert_eq!(probes.matches("HTTP/1.1 503").count(), 2);
        assert_eq!(probes.matches("HTTP/1.1 200").count(), 1);

        release.notify_one();
        let remaining = body.try_collect::<Vec<_>>().await.unwrap().concat();
        assert!(
            [first_received.as_ref(), remaining.as_ref()]
                .concat()
                .ends_with(expected_second.as_ref())
        );
        tokio::time::timeout(Duration::from_secs(1), gateway_task)
            .await
            .expect("the process did not exit after its active stream completed")
            .unwrap();

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn drain_flushes_a_backpressured_non_stream_response() {
        let provider_body = serde_json::to_vec(&json!({
            "id": "msg_large",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "a".repeat(8 * 1024 * 1024)}],
            "model": "claude-test",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }))
        .unwrap();
        let expected = provider_body.clone();
        let upstream = Router::new().route(
            "/v1/messages",
            post(move || {
                let provider_body = provider_body.clone();
                async move {
                    Response::builder()
                        .header("content-type", "application/json")
                        .body(Body::from(provider_body))
                        .unwrap()
                }
            }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_address = upstream_listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(upstream_listener, upstream).await.unwrap() });

        let mut config = test_config();
        config.anthropic_base_url = format!("http://{upstream_address}");
        config.allowed_models = vec!["claude-test".to_owned()];
        config.drain_timeout = Duration::from_secs(2);
        config.shutdown_finalize_timeout = Duration::from_millis(100);
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let lifecycle = state.lifecycle.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel();
        let mut gateway_task =
            tokio::spawn(startup::serve_with_shutdown(listener, state, async move {
                let _ = signal_rx.await;
            }));

        let response = reqwest::Client::new()
            .post(format!("http://{address}/v1/messages"))
            .header("x-api-key", "client-key")
            .header("content-type", "application/json")
            .body(r#"{"model":"claude-test","stream":false}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        signal_tx.send(()).unwrap();
        lifecycle.wait_for_draining().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(500), &mut gateway_task)
                .await
                .is_err(),
            "the process exited before the paused client resumed reading"
        );
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            expected.as_slice()
        );
        tokio::time::timeout(Duration::from_secs(1), gateway_task)
            .await
            .expect("the process did not exit after flushing the buffered response")
            .unwrap()
            .unwrap();

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn forced_shutdown_closes_a_backpressured_stream_and_records_it() {
        let provider_started = Arc::new(Notify::new());
        let provider_started_in_body = Arc::clone(&provider_started);
        let first = Bytes::from_static(
            b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_backpressure\",\"usage\":{\"input_tokens\":7}}}\n\n",
        );
        let padding = Bytes::from(format!(": {}\n\n", "x".repeat(64 * 1024)));
        let upstream = Router::new().route(
            "/v1/messages",
            post(move || {
                let provider_started = Arc::clone(&provider_started_in_body);
                let first = first.clone();
                let padding = padding.clone();
                async move {
                    let initial = stream::once(async move {
                        provider_started.notify_one();
                        Ok::<_, std::convert::Infallible>(first)
                    });
                    let repeated = stream::repeat_with(move || {
                        Ok::<_, std::convert::Infallible>(padding.clone())
                    });
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .header("request-id", "provider-backpressure")
                        .body(Body::from_stream(initial.chain(repeated)))
                        .unwrap()
                }
            }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_address = upstream_listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(upstream_listener, upstream).await.unwrap() });

        let mut config = test_config();
        config.anthropic_base_url = format!("http://{upstream_address}");
        config.allowed_models = vec!["claude-test".to_owned()];
        config.drain_timeout = Duration::from_millis(100);
        config.shutdown_finalize_timeout = Duration::from_secs(2);
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel();
        let gateway_task =
            tokio::spawn(startup::serve_with_shutdown(listener, state, async move {
                let _ = signal_rx.await;
            }));

        let body = r#"{"model":"claude-test","stream":true,"max_tokens":11}"#;
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "POST /v1/messages HTTP/1.1\r\nHost: gateway\r\nX-Api-Key: client-key\r\nX-Request-Id: w19-backpressure\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        client.write_all(request.as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), provider_started.notified())
            .await
            .expect("the provider stream was never polled");
        tokio::time::sleep(Duration::from_millis(100)).await;
        signal_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), gateway_task)
            .await
            .expect("backpressure prevented bounded shutdown")
            .unwrap()
            .unwrap();

        let mut received = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut received))
            .await
            .expect("the backpressured client connection stayed open")
            .unwrap();
        let spooled = read_spool(&spool_dir);
        assert!(spooled.contains(r#""request_id":"w19-backpressure""#));
        assert!(spooled.contains(r#""provider_request_id":"provider-backpressure""#));
        assert!(spooled.contains(r#""error_type":"shutdown_timeout""#));
        assert!(spooled.contains(r#""quota_actual_tokens":null"#));

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn forced_shutdown_before_provider_headers_keeps_the_attempt_checkpoint() {
        tokio::time::timeout(
            Duration::from_secs(30),
            exercise_shutdown_before_provider_headers(),
        )
        .await
        .expect("the shutdown checkpoint test did not finish; inspect the blocked phase");
    }

    async fn exercise_shutdown_before_provider_headers() {
        let provider_called = Arc::new(Notify::new());
        let provider_called_in_handler = Arc::clone(&provider_called);
        let upstream = Router::new().route(
            "/v1/messages",
            post(move || {
                let provider_called = Arc::clone(&provider_called_in_handler);
                async move {
                    provider_called.notify_one();
                    std::future::pending::<Response>().await
                }
            }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_address = upstream_listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(upstream_listener, upstream).await.unwrap() });

        let mut config = test_config();
        config.anthropic_base_url = format!("http://{upstream_address}");
        config.allowed_models = vec!["claude-test".to_owned()];
        config.drain_timeout = Duration::ZERO;
        config.shutdown_finalize_timeout = Duration::from_secs(2);
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel();
        let gateway_task =
            tokio::spawn(startup::serve_with_shutdown(listener, state, async move {
                let _ = signal_rx.await;
            }));

        let body = r#"{"model":"claude-test","stream":true,"max_tokens":11}"#;
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "POST /v1/messages HTTP/1.1\r\nHost: gateway\r\nX-Api-Key: client-key\r\nX-Request-Id: w19-before-headers\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        client.write_all(request.as_bytes()).await.unwrap();
        provider_called.notified().await;
        signal_tx.send(()).unwrap();
        gateway_task.await.unwrap().unwrap();

        let mut received = Vec::new();
        client.read_to_end(&mut received).await.unwrap();
        let spooled = read_spool(&spool_dir);
        assert!(spooled.contains(r#""request_id":"w19-before-headers""#));
        assert!(spooled.contains(r#""provider":"anthropic-test""#));
        assert!(spooled.contains(r#""provider_request_id":null"#));
        assert!(spooled.contains(r#""estimated_usage":{"input_tokens""#));
        assert!(spooled.contains(r#""error_type":"shutdown_timeout""#));

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn zero_drain_timeout_forces_a_durable_stream_record() {
        let first = Bytes::from_static(
            b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_force\",\"usage\":{\"input_tokens\":7}}}\n\n",
        );
        let upstream = Router::new().route(
            "/v1/messages",
            post(move || {
                let first = first.clone();
                async move {
                    let stream = async_stream::stream! {
                        yield Ok::<_, std::io::Error>(first);
                        std::future::pending::<()>().await;
                    };
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .header("request-id", "provider-force")
                        .body(Body::from_stream(stream))
                        .unwrap()
                }
            }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_address = upstream_listener.local_addr().unwrap();
        let upstream_task =
            tokio::spawn(async move { axum::serve(upstream_listener, upstream).await.unwrap() });

        let mut config = test_config();
        config.anthropic_base_url = format!("http://{upstream_address}");
        config.allowed_models = vec!["claude-test".to_owned()];
        config.drain_timeout = Duration::ZERO;
        config.shutdown_finalize_timeout = Duration::from_secs(3);
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel();
        let gateway_task = tokio::spawn(startup::serve_with_shutdown(
            listener,
            Arc::clone(&state),
            async move {
                let _ = signal_rx.await;
            },
        ));

        let response = reqwest::Client::new()
            .post(format!("http://{address}/v1/messages"))
            .header("x-api-key", "client-key")
            .header("x-request-id", "w19-force")
            .header("content-type", "application/json")
            .body(r#"{"model":"claude-test","stream":true,"max_tokens":11}"#)
            .send()
            .await
            .unwrap();
        let mut body = response.bytes_stream();
        assert!(body.next().await.unwrap().is_ok());
        signal_tx.send(()).unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), body.next()).await;
        tokio::time::timeout(Duration::from_secs(4), gateway_task)
            .await
            .expect("the forced process did not exit within its bounded cleanup window")
            .unwrap()
            .unwrap();

        let spooled = read_spool(&spool_dir);
        assert!(spooled.contains(r#""request_id":"w19-force""#));
        assert!(spooled.contains(r#""provider_request_id":"provider-force""#));
        assert!(spooled.contains(r#""provider_response_id":"msg_force""#));
        assert!(spooled.contains(r#""error_type":"shutdown_timeout""#));
        assert!(spooled.contains(r#""estimated_usage":{"input_tokens""#));
        assert!(
            spooled.contains(r#""quota_actual_tokens":null"#),
            "forced shutdown finalized quota from partial usage"
        );

        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    /// A dropped client finalizes on a background task, which turns the three
    /// reserved spool files into one ready envelope. Removing the directory
    /// while that is in flight fails with `DirectoryNotEmpty` for a reason that
    /// has nothing to do with the provider stream. Waiting on the counters
    /// alone is not enough: the reservation may not have been dropped yet when
    /// the upstream reports the closed connection.
    async fn wait_for_finalized_spool(ledger: &Ledger, directory: &std::path::Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                ledger.wait_for_finalizations().await;
                ledger.wait_for_idle().await;
                let finalized = std::fs::read_dir(directory).unwrap().all(|entry| {
                    let path = entry.unwrap().path();
                    path.file_name().and_then(|value| value.to_str()) == Some(".ledger-spool.lock")
                        || path.extension().and_then(|value| value.to_str()) == Some("ready")
                });
                if finalized {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the cancelled request was never finalized");
    }

    fn spool_files(directory: &std::path::Path) -> usize {
        std::fs::read_dir(directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                matches!(
                    entry.path().extension().and_then(|value| value.to_str()),
                    Some("ready" | "reserved")
                )
            })
            .count()
    }

    fn read_spool(directory: &std::path::Path) -> String {
        let path = std::fs::read_dir(directory)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                matches!(
                    path.extension().and_then(|value| value.to_str()),
                    Some("ready" | "reserved")
                )
            })
            .unwrap();
        String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned()
    }
}
