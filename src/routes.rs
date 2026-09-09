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
        .route("/healthz", get(health::handle))
        .merge(authenticated)
        .with_state(state)
        // Innermost first: the access log needs the id already set and the
        // status already final, so it sits inside the two id layers and
        // outside the handlers. It replaces `TraceLayer`, which reported the
        // same requests without any of the fields a consumer filters on.
        .layer(axum::middleware::from_fn(access_log))
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
    use crate::{config::Config, logging, state::AppState};

    fn test_config() -> Config {
        Config {
            listen_address: "127.0.0.1:0".parse().unwrap(),
            litellm_base_url: "http://127.0.0.1:1".to_owned(),
            litellm_master_key: "upstream-key".to_owned(),
            bootstrap_client_key: "client-key".to_owned(),
            allowed_models: vec!["company-default".to_owned()],
            otlp_endpoint: None,
        }
    }

    /// The one failure this guards against is silent: if the access log stops
    /// seeing the span, `trace_id` becomes empty, the formatter drops empty
    /// fields, and the line still looks perfectly well formed.
    #[tokio::test]
    async fn every_request_is_logged_with_a_trace_id() {
        let buffer = logging::capture::Buffer::default();
        let (subscriber, _provider) = logging::test_subscriber(buffer.clone());
        let guard = tracing::subscriber::set_default(subscriber);

        let state = Arc::new(AppState::new(test_config()));
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
}
