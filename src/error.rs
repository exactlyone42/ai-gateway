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
    #[error("streaming is not available until the SSE accounting path is enabled")]
    StreamingNotImplemented,
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
            Self::StreamingNotImplemented => "streaming_not_implemented",
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
            Self::Unauthorized | Self::ModelForbidden | Self::StreamingNotImplemented => None,
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
            Self::StreamingNotImplemented => StatusCode::NOT_IMPLEMENTED,
            Self::AuthenticationUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::ResponseBuild(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::LedgerUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        };
        let mut response = (
            status,
            Json(json!({"error": {"message": self.to_string()}})),
        )
            .into_response();
        response.extensions_mut().insert(failure);
        response
    }
}
