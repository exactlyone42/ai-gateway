mod chat_completions;
mod health;
mod messages;

pub(crate) use messages::StreamingResponse;

use std::sync::Arc;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::HeaderName,
    routing::{get, head, post},
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
    let request_max_bytes = state.config.anthropic_request_max_bytes;

    let authenticated = Router::new()
        .route("/v1/chat/completions", post(chat_completions::handle))
        .route("/v1/messages", post(messages::messages))
        .route("/v1/messages/count_tokens", post(messages::count_tokens))
        .route("/v1/models", get(messages::models))
        .layer(DefaultBodyLimit::max(request_max_bytes))
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
        .route("/api/hello", head(messages::hello))
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
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    use axum::{
        Json, Router,
        body::{Body, to_bytes},
        extract::OriginalUri,
        http::{Request, StatusCode},
        response::Response,
        routing::post,
    };
    use futures_util::StreamExt as _;
    use serde_json::json;
    use sqlx::{PgPool, Row as _};
    use tokio::sync::Notify;
    use tower::ServiceExt as _;
    use uuid::Uuid;

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
            anthropic_base_url: "http://127.0.0.1:1".to_owned(),
            anthropic_api_key: "provider-key".to_owned(),
            anthropic_request_max_bytes: 32 * 1024 * 1024,
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

    #[derive(Clone, Debug)]
    struct SeenAnthropicRequest {
        uri: String,
        authorization: Option<String>,
        api_key: Option<String>,
        version: Option<String>,
        beta: Option<String>,
        body: Vec<u8>,
    }

    #[tokio::test]
    async fn anthropic_stream_is_incremental_and_preserves_the_wire_contract() {
        let log_buffer = logging::capture::Buffer::default();
        let (subscriber, _traces) = logging::test_subscriber(log_buffer.clone());
        let log_guard = tracing::subscriber::set_default(subscriber);
        let release = Arc::new(Notify::new());
        let release_for_server = Arc::clone(&release);
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
                    let release = Arc::clone(&release_for_server);
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
                        let stream = async_stream::stream! {
                            yield Ok::<_, std::io::Error>(first);
                            release.notified().await;
                            yield Ok::<_, std::io::Error>(second);
                            std::future::pending::<()>().await;
                        };
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
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            router(state.clone()).oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages?beta=true&x=a%2Fb&x=2")
                    .header("x-api-key", "client-key")
                    .header("anthropic-version", "2023-06-01")
                    .header("anthropic-beta", "feature-a,feature-b")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body.as_slice()))
                    .unwrap(),
            ),
        )
        .await
        .expect("the gateway buffered before returning headers")
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(response.headers()["request-id"], "provider-request-1");

        let mut response_body = response.into_body().into_data_stream();
        let first_received = tokio::time::timeout(Duration::from_secs(1), response_body.next())
            .await
            .expect("the first event was buffered behind the provider barrier")
            .unwrap()
            .unwrap();
        assert_eq!(first_received.as_ref(), first.as_slice());
        assert!(
            !log_buffer
                .lines()
                .iter()
                .any(|line| line["event"]["name"] == "http.access"),
            "the request was logged complete while its body was still active"
        );
        release.notify_one();
        let second_received = tokio::time::timeout(Duration::from_secs(1), response_body.next())
            .await
            .expect("the terminal event was not relayed")
            .unwrap()
            .unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(1), response_body.next())
            .await
            .expect("the gateway waited for transport EOF after message_stop");
        assert!(ended.is_none());
        let relayed = [first_received.as_ref(), second_received.as_ref()].concat();
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
                   gr.total_latency IS NOT NULL AS total_latency_present,
                   gr.gateway_overhead IS NOT NULL AS gateway_overhead_present,
                   pa.id AS attempt_id, pa.provider_request_id,
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
