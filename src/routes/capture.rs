use std::sync::Arc;

use axum::{
    Extension,
    extract::{Path, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::{
    capture_crypto::CaptureIdentity, error::GatewayError, identity::PrincipalContext,
    state::AppState,
};

pub(crate) async fn delete(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<PrincipalContext>,
    Path(capture): Path<Uuid>,
) -> Result<StatusCode, GatewayError> {
    let Some(user) = principal.user_id else {
        return Ok(StatusCode::NOT_FOUND);
    };
    let exists = crate::capture_archive::owned_capture_exists(
        &state.pool,
        principal.tenant_id,
        user,
        capture,
    )
    .await
    .map_err(|_| GatewayError::CaptureUnavailable)?;
    if !exists {
        return Ok(StatusCode::NOT_FOUND);
    }
    let identity = CaptureIdentity {
        tenant: principal.tenant_id,
        user,
        capture,
        created_ms: 0,
        retention_days: 0,
    };
    let done = crate::capture_archive::delete_coordinated(
        &state.pool,
        &state.config.content_capture_spool_dir,
        identity,
        principal.credential_id,
        state
            .capture_erasure
            .clone()
            .ok_or(GatewayError::CaptureUnavailable)?,
        state.capture_slots.clone(),
    )
    .await
    .map_err(|_| GatewayError::CaptureUnavailable)?;
    if done {
        tokio::fs::metadata(&state.config.content_capture_spool_dir)
            .await
            .map_err(|_| GatewayError::CaptureUnavailable)?;
        let path = state
            .config
            .content_capture_spool_dir
            .join(format!("{capture}.capture"));
        match tokio::fs::symlink_metadata(path).await {
            Ok(_) => return Ok(StatusCode::ACCEPTED),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(GatewayError::CaptureUnavailable),
        }
    }
    Ok(if done {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::ACCEPTED
    })
}
