use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("invalid or missing API key")]
    Unauthorized,
    #[error("model is not allowed by policy")]
    ModelForbidden,
    #[error("upstream request failed")]
    Upstream(#[from] reqwest::Error),
    #[error("failed to build the response")]
    ResponseBuild(#[from] axum::http::Error),
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::ModelForbidden => StatusCode::FORBIDDEN,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::ResponseBuild(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            Json(json!({"error": {"message": self.to_string()}})),
        )
            .into_response()
    }
}
