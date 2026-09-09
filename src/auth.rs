use axum::http::{HeaderMap, header};
use sha2::{Digest, Sha256};

use crate::{error::GatewayError, state::AppState};

/// Accepts a caller only on an exact match with the bootstrap client key.
///
/// `skip_all` is not a style choice: the arguments carry the caller's token,
/// and a span field is written out like any other log field.
#[tracing::instrument(name = "auth.resolve", skip_all)]
pub fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), GatewayError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(GatewayError::Unauthorized)?;
    let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    if constant_time_equal(&candidate, &state.bootstrap_key_hash) {
        Ok(())
    } else {
        Err(GatewayError::Unauthorized)
    }
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
