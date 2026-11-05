mod admin;
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
    routing::{get, head, post},
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
        config::Config,
        db,
        identity::PrincipalContext,
        ledger::{Ledger, RequestCompletion, RequestStart},
        logging,
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
            openai_provider: "openai-test".to_owned(),
            openai_request_max_bytes: 32 * 1024 * 1024,
            anthropic_base_url: "http://127.0.0.1:1".to_owned(),
            anthropic_api_key: "provider-key".to_owned(),
            anthropic_auth_token: None,
            anthropic_subscription_token_file: None,
            anthropic_subscription_token_seed: None,
            anthropic_client_oauth: false,
            anthropic_oauth_credential_id: None,
            anthropic_provider: "anthropic-test".to_owned(),
            anthropic_request_max_bytes: 32 * 1024 * 1024,
            extra_provider_profiles: Vec::new(),
            stream_event_max_bytes: 32 * 1024 * 1024,
            stream_heartbeat_interval: Duration::from_secs(30),
            provider_idle_timeout: Duration::from_secs(120),
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

    #[sqlx::test]
    async fn a_successful_request_persists_request_attempt_and_usage(pool: PgPool) {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
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
                   ur.reasoning_tokens, gr.policy_decision
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
        upstream_task.abort();
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
                headers: HeaderMap::new(),
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
                headers: HeaderMap::new(),
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
                post(|| async {
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
                   pa.provider_ttft IS NOT NULL AS provider_ttft_present
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
                    entry
                        .unwrap()
                        .path()
                        .extension()
                        .and_then(|value| value.to_str())
                        == Some("ready")
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
