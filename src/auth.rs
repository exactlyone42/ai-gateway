use axum::http::{HeaderMap, header};
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
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(GatewayError::Unauthorized)?;
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

fn constant_time_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_equal;

    #[test]
    fn constant_time_key_comparison_works() {
        assert!(constant_time_equal(&[1; 32], &[1; 32]));
        assert!(!constant_time_equal(&[1; 32], &[2; 32]));
    }
}
