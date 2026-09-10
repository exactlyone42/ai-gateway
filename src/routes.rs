mod chat_completions;
mod health;

use std::sync::Arc;

use axum::{
    Router,
    http::HeaderName,
    routing::{get, post},
};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};

use crate::{
    middleware::{REQUEST_ID_HEADER, access_log, require_client_key},
    state::AppState,
};

/// The route table: which endpoints exist, which of them need a client key,
/// and the layers every request crosses.
pub fn router(state: Arc<AppState>) -> Router {
    let request_id_header = HeaderName::from_static(REQUEST_ID_HEADER);

    let authenticated = Router::new()
        .route("/v1/chat/completions", post(chat_completions::handle))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_client_key,
        ));

    Router::new()
        // `/healthz` is the name the old single endpoint had. Keeping it as
        // an alias means existing callers do not break on this change.
        .route("/healthz", get(health::live))
        .route("/health", get(health::live))
        .route("/ready", get(health::ready))
        .merge(authenticated)
        .with_state(state.clone())
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

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use axum::{
        Json, Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        routing::post,
    };
    use serde_json::json;
    use sqlx::{PgPool, Row as _};
    use tower::ServiceExt as _;

    use super::router;
    use chrono::Utc;

    use crate::{
        config::Config,
        db,
        identity::PrincipalContext,
        ledger::{Ledger, RequestCompletion, RequestStart},
        logging,
        state::AppState,
    };

    fn test_config() -> Config {
        Config {
            listen_address: "127.0.0.1:0".parse().unwrap(),
            litellm_base_url: "http://127.0.0.1:1".to_owned(),
            litellm_master_key: "upstream-key".to_owned(),
            bootstrap_client_key: Some("client-key".to_owned()),
            allowed_models: vec!["company-default".to_owned()],
            otlp_endpoint: None,
            // Port 1 has no listener. The pool is lazy, so building it costs
            // nothing; any test that actually queries would fail, which is
            // what we want -- these tests must not depend on a live database.
            database_url: "postgres://gateway@127.0.0.1:1/gateway".to_owned(),
            database_max_connections: 1,
            credential_hash_key: b"test-credential-hash-key-at-least-32".to_vec(),
            credential_hash_key_id: "v1".to_owned(),
            credential_retiring_hash_keys: Vec::new(),
            credential_environment: "dev".to_owned(),
            credential_cache_capacity: 8,
            credential_cache_ttl_seconds: 60,
            upstream_provider: "configured-upstream".to_owned(),
            ledger_spool_dir: std::env::temp_dir().join(format!(
                "agent-pool-gateway-route-test-{}",
                uuid::Uuid::new_v4()
            )),
            ledger_spool_max_bytes: 1024 * 1024,
            ledger_spool_entry_max_bytes: 32 * 1024,
        }
    }

    /// Builds the state these tests share: real router, no live database.
    fn test_state() -> Arc<AppState> {
        let config = test_config();
        let pool = db::pool(&config.database_url, config.database_max_connections)
            .expect("the test connection string is valid");
        Arc::new(AppState::new(config, pool).unwrap())
    }

    /// The one failure this guards against is silent: if the access log stops
    /// seeing the span, `trace_id` becomes empty, the formatter drops empty
    /// fields, and the line still looks perfectly well formed.
    #[tokio::test]
    async fn every_request_is_logged_with_a_trace_id() {
        let buffer = logging::capture::Buffer::default();
        let (subscriber, _provider) = logging::test_subscriber(buffer.clone());
        let guard = tracing::subscriber::set_default(subscriber);

        let state = test_state();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(guard);

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
        let guard = tracing::subscriber::set_default(subscriber);

        let state = test_state();
        let response = router(state).oneshot(request).await.unwrap();
        let status = response.status();
        drop(guard);

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
        config.litellm_base_url = format!("http://{address}");
        config.upstream_provider = "test-provider".to_owned();
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
                   ur.reasoning_tokens
            FROM gateway_requests gr
            JOIN provider_attempts pa
              ON (pa.tenant_id, pa.gateway_request_id) = (gr.tenant_id, gr.id)
            JOIN usage_records ur
              ON (ur.tenant_id, ur.gateway_request_id, ur.provider_attempt_id) =
                 (pa.tenant_id, pa.gateway_request_id, pa.id)
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
        assert_eq!(row.get::<i64, _>("input_tokens"), 10);
        assert_eq!(row.get::<i64, _>("output_tokens"), 3);
        assert_eq!(row.get::<i64, _>("cached_input_tokens"), 4);
        assert_eq!(row.get::<i64, _>("cache_write_input_tokens"), 2);
        assert_eq!(row.get::<i64, _>("reasoning_tokens"), 1);
        upstream_task.abort();
        std::fs::remove_dir_all(&state.config.ledger_spool_dir).unwrap();
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
        config.litellm_base_url = format!("http://{address}");
        config.upstream_provider = "test-provider".to_owned();
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
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM usage_records")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
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
        config.litellm_base_url = format!("http://{address}");
        config.ledger_spool_max_bytes = 64 * 1024;
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
    async fn streaming_is_rejected_before_the_upstream_until_w05_accounts_for_it() {
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
        config.litellm_base_url = format!("http://{address}");
        let spool_dir = config.ledger_spool_dir.clone();
        let pool = db::pool(&config.database_url, config.database_max_connections).unwrap();
        let state = Arc::new(AppState::new(config, pool).unwrap());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer client-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"company-default","stream":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        assert!(!called.load(Ordering::Acquire), "the upstream was called");
        upstream_task.abort();
        std::fs::remove_dir_all(spool_dir).unwrap();
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
