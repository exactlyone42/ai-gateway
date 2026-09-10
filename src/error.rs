use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use thiserror::Error;

use crate::{identity::IdentityError, ledger::LedgerError};

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("invalid or missing API key")]
    Unauthorized,
    #[error("model is not allowed by policy")]
    ModelForbidden,
    #[error("request body is not valid JSON")]
    InvalidRequest(#[source] serde_json::Error),
    #[error("request option is not supported by this gateway")]
    UnsupportedRequest,
    #[error("request body exceeds the configured endpoint limit")]
    PayloadTooLarge,
    #[error("request body could not be read")]
    RequestBodyUnavailable(String),
    #[error("provider configuration is invalid")]
    InvalidConfiguration,
    #[error("authentication service is unavailable")]
    AuthenticationUnavailable(#[source] IdentityError),
    #[error("upstream request failed")]
    Upstream(#[from] reqwest::Error),
    #[error("failed to build the response")]
    ResponseBuild(#[from] axum::http::Error),
    #[error("usage ledger is unavailable")]
    LedgerUnavailable(#[from] LedgerError),
}

/// Carries the failure from the error boundary to the access log, which is the
/// single place that writes a log line.
///
/// The type is a stable identifier; the cause is the sentence a person needs
/// at 03:00 -- without it a 502 says only "upstream failed", never whether it
/// was DNS, TLS or a timeout.
#[derive(Clone)]
pub struct ErrorType {
    pub kind: &'static str,
    pub cause: Option<String>,
}

impl GatewayError {
    /// Stable identifier for logs and metrics. Never the message text: that
    /// is written for humans and changes without notice.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::ModelForbidden => "model_forbidden",
            Self::InvalidRequest(_) => "invalid_request",
            Self::UnsupportedRequest => "unsupported_request",
            Self::PayloadTooLarge => "payload_too_large",
            Self::RequestBodyUnavailable(_) => "request_body_unavailable",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::AuthenticationUnavailable(_) => "authentication_unavailable",
            Self::Upstream(_) => "upstream_unavailable",
            Self::ResponseBuild(_) => "response_build_failed",
            Self::LedgerUnavailable(_) => "ledger_unavailable",
        }
    }

    /// Only for the variants that wrap someone else's failure. A rejected key
    /// or a disallowed model needs no cause: the type says everything, and
    /// the request that caused it is the caller's own.
    fn cause(&self) -> Option<String> {
        match self {
            Self::Unauthorized
            | Self::ModelForbidden
            | Self::UnsupportedRequest
            | Self::PayloadTooLarge
            | Self::InvalidConfiguration => None,
            Self::InvalidRequest(error) => Some(error.to_string()),
            Self::RequestBodyUnavailable(error) => Some(error.clone()),
            Self::AuthenticationUnavailable(error) => Some(error.to_string()),
            Self::Upstream(error) => Some(error.to_string()),
            Self::ResponseBuild(error) => Some(error.to_string()),
            Self::LedgerUnavailable(error) => Some(error.to_string()),
        }
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let failure = ErrorType {
            kind: self.kind(),
            cause: self.cause(),
        };
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::ModelForbidden => StatusCode::FORBIDDEN,
            Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::UnsupportedRequest => StatusCode::BAD_REQUEST,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RequestBodyUnavailable(_) => StatusCode::BAD_REQUEST,
            Self::InvalidConfiguration => StatusCode::INTERNAL_SERVER_ERROR,
            Self::AuthenticationUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::ResponseBuild(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::LedgerUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        };
        let mut response = (
            status,
            Json(json!({"error": {"code": failure.kind, "message": self.to_string()}})),
        )
            .into_response();
        response.extensions_mut().insert(failure);
        response
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::to_bytes, response::IntoResponse as _};

    use super::GatewayError;

    #[tokio::test]
    async fn gateway_errors_expose_the_stable_code_in_the_response() {
        let response = GatewayError::Unauthorized.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "unauthorized");
    }
}
