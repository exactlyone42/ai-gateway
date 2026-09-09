use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::json;

/// `GET /healthz` -- liveness only, no dependency checks.
pub async fn handle() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "ok"})))
}
