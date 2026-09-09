use std::sync::Arc;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::{auth, error::GatewayError, state::AppState};

/// Turns a request away before it reaches any handler unless it carries a
/// valid client key.
pub async fn require_client_key(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, GatewayError> {
    auth::authorize(&state, request.headers())?;
    Ok(next.run(request).await)
}
