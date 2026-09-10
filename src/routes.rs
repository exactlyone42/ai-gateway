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
    use std::sync::Arc;

    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt as _;

    use super::router;
    use crate::{config::Config, db, logging, state::AppState};

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
        }
    }

    /// Builds the state these tests share: real router, no live database.
    fn test_state() -> Arc<AppState> {
        let config = test_config();
        let pool = db::pool(&config.database_url, config.database_max_connections)
            .expect("the test connection string is valid");
        Arc::new(AppState::new(config, pool))
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
}
