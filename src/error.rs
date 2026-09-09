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

/// Carries the error type from the error boundary to the access log, which is
/// the single place that writes a log line.
#[derive(Clone, Copy)]
pub struct ErrorType(pub &'static str);

impl GatewayError {
    /// Stable identifier for logs and metrics. Never the message text: that
    /// is written for humans and changes without notice.
    fn error_type(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::ModelForbidden => "model_forbidden",
            Self::Upstream(_) => "upstream_unavailable",
            Self::ResponseBuild(_) => "response_build_failed",
        }
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let error_type = self.error_type();
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::ModelForbidden => StatusCode::FORBIDDEN,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::ResponseBuild(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let mut response = (
            status,
            Json(json!({"error": {"message": self.to_string()}})),
        )
            .into_response();
        response.extensions_mut().insert(ErrorType(error_type));
        response
    }
}
