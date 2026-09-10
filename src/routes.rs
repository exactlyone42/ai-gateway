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
    middleware::{REQUEST_ID_HEADER, access_log, require_client_key},
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
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    use axum::{
        Json, Router,
        body::{Body, Bytes, to_bytes},
        extract::OriginalUri,
        http::{HeaderMap, Request, StatusCode},
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

    pub(super) fn test_config() -> Config {
        Config {
            listen_address: "127.0.0.1:0".parse().unwrap(),
            openai_base_url: "http://127.0.0.1:1".to_owned(),
            openai_api_key: "upstream-key".to_owned(),
            openai_provider: "openai-test".to_owned(),
            openai_request_max_bytes: 32 * 1024 * 1024,
            anthropic_base_url: "http://127.0.0.1:1".to_owned(),
            anthropic_api_key: "provider-key".to_owned(),
            anthropic_provider: "anthropic-test".to_owned(),
            anthropic_request_max_bytes: 32 * 1024 * 1024,
            stream_event_max_bytes: 32 * 1024 * 1024,
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
        }
    }

    /// Builds the state these tests share: real router, no live database.
    fn test_state() -> Arc<AppState> {
        let config = test_config();
        let pool = db::pool(&config.database_url, config.database_max_connections)
            .expect("the test connection string is valid");
        Arc::new(AppState::new(config, pool).unwrap())
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
                   ur.reasoning_tokens
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
        let upstream = Router::new().route(
            "/v1/responses",
            post(|| async {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("content-type", "application/json"), ("retry-after", "11")],
                    Json(json!({
                        "error": {
                            "code": "rate_limit_exceeded",
                            "message": "try later",
                            "type": "rate_limit_error"
                        }
                    })),
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
                    .body(Body::from(r#"{"model":"company-default","input":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "11");
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "rate_limit_exceeded");

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
        let upstream = Router::new().route(
            "/v1/responses",
            post(|| async {
                (
                    [("content-type", "text/event-stream")],
                    "{\"type\":\"response.completed\"}\n\n",
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
        let keepalive_received = tokio::time::timeout(Duration::from_secs(1), response_body.next())
            .await
            .expect("the first event was buffered behind the provider barrier")
            .unwrap()
            .unwrap();
        let start_received = tokio::time::timeout(Duration::from_secs(1), response_body.next())
            .await
            .expect("the start event was buffered behind the provider barrier")
            .unwrap()
            .unwrap();
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
        release.notify_one();
        let delta_received = tokio::time::timeout(Duration::from_secs(1), response_body.next())
            .await
            .expect("the delta event was not relayed")
            .unwrap()
            .unwrap();
        let terminal_received = tokio::time::timeout(Duration::from_secs(1), response_body.next())
            .await
            .expect("the terminal event was not relayed")
            .unwrap()
            .unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(1), response_body.next())
            .await
            .expect("the gateway waited for transport EOF after message_stop");
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
        assert_eq!(row.get::<i64, _>("input_tokens"), 12);
        assert_eq!(row.get::<i64, _>("output_tokens"), 5);
        assert_eq!(row.get::<i64, _>("cached_input_tokens"), 4);
        assert_eq!(row.get::<i64, _>("reasoning_tokens"), 2);

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
