use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde_json::json;

use crate::{db, state::AppState};

/// `GET /health` -- liveness. The process is running and can answer.
///
/// Deliberately checks nothing else. An orchestrator restarts a container
/// whose liveness fails, so making this depend on the database would turn a
/// database outage into a restart loop of a process that was working fine.
pub async fn live() -> impl IntoResponse {
    // The revision is here so a deployment can prove which build is serving,
    // rather than take someone's word for it. It is a build identifier, not a
    // secret, and this endpoint is already reachable without a client key.
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "revision": env!("BUILD_REVISION"),
        })),
    )
}

/// `GET /ready` -- readiness. The process can serve traffic right now.
///
/// This is the endpoint a load balancer polls to decide whether to send
/// requests here, so it does check the dependencies a request needs.
pub async fn ready(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if state.lifecycle.is_draining() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "not_ready", "process": "draining"})),
        );
    }
    match db::is_reachable(&state.pool).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"status": "ready", "database": "ok"})),
        ),
        Err(error) => {
            // Readiness failures are the signal that traffic should go
            // elsewhere, so they are never silent. The cause names which
            // dependency, which is the whole point of a separate endpoint.
            tracing::warn!(
                error.type = "database_unreachable",
                exception.message = %error,
                "not ready to serve traffic"
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"status": "not_ready", "database": "unreachable"})),
            )
        }
    }
}
