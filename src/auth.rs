use axum::http::{HeaderMap, header};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};

use crate::{
    error::GatewayError,
    identity::{IdentityError, PrincipalContext},
    state::AppState,
};

/// Resolves a development bootstrap key or a tenant-scoped database credential.
///
/// `skip_all` is not a style choice: the arguments carry the caller's token,
/// and a span field is written out like any other log field.
#[tracing::instrument(name = "auth.resolve", skip_all)]
pub async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<PrincipalContext, GatewayError> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let api_key = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok());
    let token = match (bearer, api_key) {
        (Some(left), Some(right)) if !secret_equal(left, right) => {
            return Err(GatewayError::Unauthorized);
        }
        (Some(value), _) | (_, Some(value)) => value,
        (None, None) => return Err(GatewayError::Unauthorized),
    };
    if state.config.anthropic_client_oauth && token.starts_with("sk-ant-oat") {
        validate_anthropic_oauth(state, token).await?;
        return state
            .credentials
            .resolve_by_id(
                state
                    .config
                    .anthropic_oauth_credential_id
                    .ok_or(GatewayError::InvalidConfiguration)?,
            )
            .await
            .map_err(|error| match error {
                IdentityError::InvalidCredential => GatewayError::Unauthorized,
                other => GatewayError::AuthenticationUnavailable(other),
            });
    }
    let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    if state
        .bootstrap_key_hash
        .as_ref()
        .is_some_and(|expected| constant_time_equal(&candidate, expected))
    {
        return Ok(PrincipalContext::bootstrap());
    }
    state
        .credentials
        .resolve(token)
        .await
        .map_err(|error| match error {
            IdentityError::InvalidCredential => GatewayError::Unauthorized,
            other => GatewayError::AuthenticationUnavailable(other),
        })
}

async fn validate_anthropic_oauth(state: &AppState, token: &str) -> Result<(), GatewayError> {
    let response = state
        .client
        .get(format!(
            "{}/v1/models?limit=1",
            state.config.anthropic_base_url
        ))
        .bearer_auth(token)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await?;
    match response.status() {
        StatusCode::OK => Ok(()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(GatewayError::Unauthorized),
        _ => Err(GatewayError::ProviderUnavailable),
    }
}

fn secret_equal(left: &str, right: &str) -> bool {
    let left: [u8; 32] = Sha256::digest(left.as_bytes()).into();
    let right: [u8; 32] = Sha256::digest(right.as_bytes()).into();
    constant_time_equal(&left, &right)
}

fn constant_time_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::{constant_time_equal, secret_equal};

    #[test]
    fn constant_time_key_comparison_works() {
        assert!(constant_time_equal(&[1; 32], &[1; 32]));
        assert!(!constant_time_equal(&[1; 32], &[2; 32]));
    }

    #[test]
    fn duplicate_auth_headers_must_name_the_same_secret() {
        assert!(secret_equal("same", "same"));
        assert!(!secret_equal("left", "right"));
    }
}
