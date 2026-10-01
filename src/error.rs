use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use thiserror::Error;

use crate::{
    identity::IdentityError, ledger::LedgerError, model_registry::RegistryError,
    policy::PolicyError, quota::QuotaError, scheduler::SchedulerError,
};

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("invalid or missing API key")]
    Unauthorized,
    #[error("model is not allowed by policy")]
    ModelForbidden,
    #[error("request policy hint is invalid")]
    InvalidPolicyHint,
    #[error("request denied by policy")]
    PolicyDenied,
    #[error("request exceeds an active quota")]
    QuotaExceeded,
    #[error("request is deferred by a soft quota")]
    QuotaDeferred,
    #[error("scheduler queue is full")]
    QueueFull,
    #[error("request expired while waiting for provider capacity")]
    QueueExpired,
    #[error("scheduler is shutting down")]
    SchedulerClosed,
    #[error("gateway is draining for shutdown")]
    ProcessDraining,
    #[error("no provider is currently available")]
    ProviderUnavailable,
    #[error("model cannot satisfy required capability: {0}")]
    CapabilityMismatch(&'static str),
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
    #[error("upstream response does not satisfy the translated protocol")]
    UpstreamProtocol,
    #[error("failed to build the response")]
    ResponseBuild(#[from] axum::http::Error),
    #[error("usage ledger is unavailable")]
    LedgerUnavailable(#[from] LedgerError),
    #[error("model registry is unavailable")]
    RegistryUnavailable(#[source] RegistryError),
    #[error("policy service is unavailable")]
    PolicyUnavailable(#[source] PolicyError),
    #[error("quota service is unavailable")]
    QuotaUnavailable(#[source] QuotaError),
    #[error("scheduler is unavailable")]
    SchedulerUnavailable(#[source] SchedulerError),
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
            Self::InvalidPolicyHint => "invalid_policy_hint",
            Self::PolicyDenied => "policy_denied",
            Self::QuotaExceeded => "quota_exceeded",
            Self::QuotaDeferred => "quota_deferred",
            Self::QueueFull => "scheduler_queue_full",
            Self::QueueExpired => "scheduler_queue_expired",
            Self::SchedulerClosed => "scheduler_closed",
            Self::ProcessDraining => "process_draining",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::CapabilityMismatch(_) => "model_capability_mismatch",
            Self::InvalidRequest(_) => "invalid_request",
            Self::UnsupportedRequest => "unsupported_request",
            Self::PayloadTooLarge => "payload_too_large",
            Self::RequestBodyUnavailable(_) => "request_body_unavailable",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::AuthenticationUnavailable(_) => "authentication_unavailable",
            Self::Upstream(_) => "upstream_unavailable",
            Self::UpstreamProtocol => "upstream_protocol_error",
            Self::ResponseBuild(_) => "response_build_failed",
            Self::LedgerUnavailable(_) => "ledger_unavailable",
            Self::RegistryUnavailable(_) => "model_registry_unavailable",
            Self::PolicyUnavailable(_) => "policy_unavailable",
            Self::QuotaUnavailable(_) => "quota_unavailable",
            Self::SchedulerUnavailable(_) => "scheduler_unavailable",
        }
    }

    /// Only for the variants that wrap someone else's failure. A rejected key
    /// or a disallowed model needs no cause: the type says everything, and
    /// the request that caused it is the caller's own.
    fn cause(&self) -> Option<String> {
        match self {
            Self::Unauthorized
            | Self::ModelForbidden
            | Self::InvalidPolicyHint
            | Self::PolicyDenied
            | Self::QuotaExceeded
            | Self::QuotaDeferred
            | Self::QueueFull
            | Self::QueueExpired
            | Self::SchedulerClosed
            | Self::ProcessDraining
            | Self::ProviderUnavailable
            | Self::CapabilityMismatch(_)
            | Self::UnsupportedRequest
            | Self::PayloadTooLarge
            | Self::InvalidConfiguration => None,
            Self::InvalidRequest(error) => Some(error.to_string()),
            Self::RequestBodyUnavailable(error) => Some(error.clone()),
            Self::AuthenticationUnavailable(error) => Some(error.to_string()),
            Self::Upstream(error) => Some(error.to_string()),
            Self::UpstreamProtocol => None,
            Self::ResponseBuild(error) => Some(error.to_string()),
            Self::LedgerUnavailable(error) => Some(error.to_string()),
            Self::RegistryUnavailable(error) => Some(error.to_string()),
            Self::PolicyUnavailable(error) => Some(error.to_string()),
            Self::QuotaUnavailable(error) => Some(error.to_string()),
            Self::SchedulerUnavailable(error) => Some(error.to_string()),
        }
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let failure = ErrorType {
            kind: self.kind(),
            cause: self.cause(),
        };
        let status = match &self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::ModelForbidden => StatusCode::FORBIDDEN,
            Self::InvalidPolicyHint => StatusCode::BAD_REQUEST,
            Self::PolicyDenied => StatusCode::FORBIDDEN,
            Self::QuotaExceeded | Self::QuotaDeferred => StatusCode::TOO_MANY_REQUESTS,
            Self::QueueFull => StatusCode::TOO_MANY_REQUESTS,
            Self::QueueExpired => StatusCode::REQUEST_TIMEOUT,
            Self::SchedulerClosed | Self::ProcessDraining => StatusCode::SERVICE_UNAVAILABLE,
            Self::ProviderUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::CapabilityMismatch(_) => StatusCode::BAD_REQUEST,
            Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::UnsupportedRequest => StatusCode::BAD_REQUEST,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RequestBodyUnavailable(_) => StatusCode::BAD_REQUEST,
            Self::InvalidConfiguration => StatusCode::INTERNAL_SERVER_ERROR,
            Self::AuthenticationUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::UpstreamProtocol => StatusCode::BAD_GATEWAY,
            Self::ResponseBuild(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::LedgerUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::RegistryUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::PolicyUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::QuotaUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::SchedulerUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        };
        let mut response = (
            status,
            Json(json!({"error": {"code": failure.kind, "message": self.to_string()}})),
        )
            .into_response();
        response.extensions_mut().insert(failure);
        if matches!(
            self,
            Self::QueueFull | Self::SchedulerClosed | Self::ProcessDraining
        ) {
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("1"),
            );
        }
        response
    }
}

impl From<PolicyError> for GatewayError {
    fn from(error: PolicyError) -> Self {
        match error {
            PolicyError::InvalidHint => Self::InvalidPolicyHint,
            error @ (PolicyError::InvalidTenant
            | PolicyError::InvalidDocument(_)
            | PolicyError::InvalidValue
            | PolicyError::CacheUnavailable
            | PolicyError::Database(_)) => Self::PolicyUnavailable(error),
        }
    }
}

impl From<RegistryError> for GatewayError {
    fn from(error: RegistryError) -> Self {
        match error {
            RegistryError::InvalidSelector | RegistryError::ModelUnavailable => {
                Self::ModelForbidden
            }
            RegistryError::CapabilityMismatch(capability) => Self::CapabilityMismatch(capability),
            error @ (RegistryError::UntrustedProvider
            | RegistryError::InvalidProviderProfile
            | RegistryError::RequestEncoding(_)
            | RegistryError::Database(_)) => Self::RegistryUnavailable(error),
        }
    }
}

impl From<QuotaError> for GatewayError {
    fn from(error: QuotaError) -> Self {
        Self::QuotaUnavailable(error)
    }
}

impl From<SchedulerError> for GatewayError {
    fn from(error: SchedulerError) -> Self {
        match error {
            SchedulerError::QueueFull => Self::QueueFull,
            SchedulerError::HardQuota => Self::QuotaExceeded,
            SchedulerError::Expired => Self::QueueExpired,
            SchedulerError::Closed => Self::SchedulerClosed,
            error @ (SchedulerError::InvalidConfiguration | SchedulerError::Unavailable) => {
                Self::SchedulerUnavailable(error)
            }
        }
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
