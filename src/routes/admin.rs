use std::{net::SocketAddr, sync::Arc};

use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, MatchedPath, Path, Query, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::Response,
    routing::{delete, get, post},
};
use tower_http::cors::CorsLayer;

use crate::{
    admin::{
        AdminAccountCreate, AdminAccountPatch, AdminAccountSecretOutput, AdminContext, AdminError,
        BootstrapInput, BootstrapOutput, CursorPage, DeleteQuery, IncidentCreate,
        IncidentEventCreate, IncidentListQuery, IncidentPatch, LifecycleInput, ListQuery,
        ReportListQuery, ReportWindowQuery, RequestListQuery, ResourceKind, ResourceMutationOutput,
        ResourcePage, ResourceTarget, RotateCredentialInput, SessionOutput, UsageQuery, authorize,
    },
    middleware::REQUEST_ID_HEADER,
    state::AppState,
};

pub fn router(state: &Arc<AppState>) -> Router<Arc<AppState>> {
    let protected = Router::new()
        .route("/admin/v1/session", delete(logout))
        .route("/admin/v1/overview", get(overview))
        .route(
            "/admin/v1/task-classification-shadow",
            get(task_classification_shadow),
        )
        .route(
            "/admin/v1/task-decomposition-shadow",
            get(task_decomposition_shadow),
        )
        .route("/admin/v1/identity-map", get(identity_map))
        .route("/admin/v1/requests", get(request_page))
        .route("/admin/v1/requests/{id}", get(request_report))
        .route("/admin/v1/request-captures/{id}", get(capture_report))
        .route(
            "/admin/v1/request-captures/{id}/content",
            get(capture_content),
        )
        .route("/admin/v1/usage", get(usage_report))
        .route("/admin/v1/audit", get(audit_page))
        .route("/admin/v1/catalog", get(catalog_report))
        .route("/admin/v1/governance", get(governance_report))
        .route("/admin/v1/system-health", get(system_health_report))
        .route(
            "/admin/v1/incidents",
            get(list_incidents).post(create_incident),
        )
        .route(
            "/admin/v1/incidents/{id}",
            get(get_incident).patch(update_incident),
        )
        .route(
            "/admin/v1/incidents/{id}/events",
            post(append_incident_event),
        )
        .route("/admin/v1/credentials/{id}/rotate", post(rotate_credential))
        .route(
            "/admin/v1/provider-tokens/anthropic",
            axum::routing::put(replace_anthropic_token),
        )
        .route(
            "/admin/v1/provider-tokens/anthropic/{user_id}",
            get(personal_anthropic_status)
                .put(replace_personal_anthropic_token)
                .delete(revoke_personal_anthropic_token),
        )
        .route(
            "/admin/v1/provider-api-keys/{provider}/{user_id}",
            get(personal_api_key_status)
                .put(replace_personal_api_key)
                .delete(revoke_personal_api_key),
        )
        .route(
            "/admin/v1/{resource}/{id}/lifecycle",
            post(set_registry_lifecycle),
        )
        .route(
            "/admin/v1/admin-accounts",
            get(list_admin_accounts).post(create_admin_account),
        )
        .route(
            "/admin/v1/admin-accounts/{id}",
            axum::routing::patch(update_admin_account),
        )
        .route(
            "/admin/v1/{resource}",
            get(list_resources).post(create_resource),
        )
        .route(
            "/admin/v1/{resource}/{id}",
            get(get_resource)
                .patch(update_resource)
                .delete(delete_resource),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_admin_session,
        ));
    Router::new()
        .route("/admin/v1/bootstrap", post(bootstrap))
        .route("/admin/v1/sessions", post(login))
        .merge(protected)
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(cors(state.config.admin_allowed_origin.as_deref()))
}

async fn list_incidents(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Query(query): Query<IncidentListQuery>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(state.admin.list_incidents(&context, query).await?))
}

async fn get_incident(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(state.admin.get_incident(&context, id).await?))
}

async fn create_incident(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<IncidentCreate>,
) -> Result<(StatusCode, Json<serde_json::Value>), AdminError> {
    let source = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    let output = state
        .admin
        .create_incident(&context, input, source, peer.ip(), request_id(&headers)?)
        .await?;
    Ok((StatusCode::CREATED, Json(output)))
}

async fn update_incident(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    Json(input): Json<IncidentPatch>,
) -> Result<Json<serde_json::Value>, AdminError> {
    let source = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    Ok(Json(
        state
            .admin
            .update_incident(
                &context,
                id,
                input,
                source,
                peer.ip(),
                request_id(&headers)?,
            )
            .await?,
    ))
}

async fn append_incident_event(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    Json(input): Json<IncidentEventCreate>,
) -> Result<(StatusCode, Json<serde_json::Value>), AdminError> {
    let source = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    let output = state
        .admin
        .append_incident_event(
            &context,
            id,
            input,
            source,
            peer.ip(),
            request_id(&headers)?,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(output)))
}

async fn overview(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Query(query): Query<ReportWindowQuery>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(state.admin.overview_report(&context, query).await?))
}

async fn task_classification_shadow(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Query(query): Query<ReportWindowQuery>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(
        state
            .admin
            .task_classification_shadow_report(&context, query)
            .await?,
    ))
}

async fn task_decomposition_shadow(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Query(query): Query<ReportWindowQuery>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(
        state
            .admin
            .task_decomposition_shadow_report(&context, query)
            .await?,
    ))
}

async fn identity_map(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(state.admin.identity_map(&context).await?))
}

async fn request_page(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Query(query): Query<RequestListQuery>,
) -> Result<Json<CursorPage>, AdminError> {
    Ok(Json(state.admin.request_page(&context, query).await?))
}

async fn capture_report(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(state.admin.capture_report(&context, id).await?))
}

#[derive(serde::Deserialize)]
struct CaptureContentQuery {
    leg: String,
}

async fn capture_content(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Path(id): Path<uuid::Uuid>,
    Query(query): Query<CaptureContentQuery>,
) -> Result<Response, AdminError> {
    use crate::capture_crypto::CaptureLeg;
    let leg = match query.leg.as_str() {
        "client_request" => CaptureLeg::ClientRequest,
        "provider_request" => CaptureLeg::ProviderRequest,
        "provider_response" => CaptureLeg::ProviderResponse,
        "client_response" => CaptureLeg::ClientResponse,
        _ => return Err(AdminError::InvalidRequest),
    };
    let body = crate::capture_archive::admin_content(state, context, id, leg)
        .await
        .map_err(|error| match error {
            crate::capture_archive::AdminCaptureError::NotFound => AdminError::NotFound,
            crate::capture_archive::AdminCaptureError::Unavailable => AdminError::Unavailable,
        })?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .body(body)
        .map_err(|_| AdminError::Unavailable)
}

async fn request_report(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(state.admin.request_report(&context, id).await?))
}

async fn usage_report(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Query(query): Query<UsageQuery>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(state.admin.usage_report(&context, query).await?))
}

async fn audit_page(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Query(query): Query<ReportListQuery>,
) -> Result<Json<CursorPage>, AdminError> {
    Ok(Json(state.admin.audit_page(&context, query).await?))
}

async fn catalog_report(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(state.admin.catalog_report(&context).await?))
}

async fn governance_report(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(state.admin.governance_report(&context).await?))
}

async fn system_health_report(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(
        state
            .admin
            .system_health_report(&context, state.config.otlp_endpoint.is_some())
            .await?,
    ))
}

async fn bootstrap(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<BootstrapInput>,
) -> Result<(StatusCode, Json<BootstrapOutput>), AdminError> {
    let token = bearer(&headers)?;
    let source_ip = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    let output = state
        .admin
        .bootstrap(token, input, source_ip, peer.ip(), request_id(&headers)?)
        .await?;
    Ok((StatusCode::CREATED, Json(output)))
}

async fn login(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<SessionOutput>), AdminError> {
    let source_ip = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    let output = state
        .admin
        .login(
            bearer(&headers)?,
            source_ip,
            peer.ip(),
            request_id(&headers)?,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(output)))
}

async fn logout(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<StatusCode, AdminError> {
    let source_ip = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    state
        .admin
        .logout(&context, source_ip, peer.ip(), request_id(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_admin_accounts(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ResourcePage>, AdminError> {
    Ok(Json(
        state.admin.list_admin_accounts(&context, query).await?,
    ))
}

async fn create_admin_account(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<AdminAccountCreate>,
) -> Result<(StatusCode, Json<AdminAccountSecretOutput>), AdminError> {
    let source = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    let output = state
        .admin
        .create_admin_account(&context, input, source, peer.ip(), request_id(&headers)?)
        .await?;
    Ok((StatusCode::CREATED, Json(output)))
}

async fn update_admin_account(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    Json(input): Json<AdminAccountPatch>,
) -> Result<Json<serde_json::Value>, AdminError> {
    let source = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    Ok(Json(
        state
            .admin
            .update_admin_account(
                &context,
                id,
                input,
                source,
                peer.ip(),
                request_id(&headers)?,
            )
            .await?,
    ))
}

async fn list_resources(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Path(resource): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ResourcePage>, AdminError> {
    let kind = resource.parse::<ResourceKind>()?;
    Ok(Json(
        state.admin.list_resources(&context, kind, query).await?,
    ))
}

async fn get_resource(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Path((resource, id)): Path<(String, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, AdminError> {
    let kind = resource.parse::<ResourceKind>()?;
    Ok(Json(state.admin.get_resource(&context, kind, id).await?))
}

async fn create_resource(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(resource): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<ResourceMutationOutput>), AdminError> {
    let kind = resource.parse::<ResourceKind>()?;
    let source = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    let output = state
        .admin
        .create_resource(
            &context,
            kind,
            payload,
            source,
            peer.ip(),
            request_id(&headers)?,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(output)))
}

async fn update_resource(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((resource, id)): Path<(String, uuid::Uuid)>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<ResourceMutationOutput>, AdminError> {
    let kind = resource.parse::<ResourceKind>()?;
    let source = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    Ok(Json(
        state
            .admin
            .update_resource(
                &context,
                ResourceTarget::new(kind, id),
                payload,
                source,
                peer.ip(),
                request_id(&headers)?,
            )
            .await?,
    ))
}

async fn delete_resource(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((resource, id)): Path<(String, uuid::Uuid)>,
    Query(query): Query<DeleteQuery>,
    headers: HeaderMap,
) -> Result<Json<ResourceMutationOutput>, AdminError> {
    let kind = resource.parse::<ResourceKind>()?;
    let source = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    Ok(Json(
        state
            .admin
            .delete_resource(
                &context,
                ResourceTarget::new(kind, id),
                query.version,
                source,
                peer.ip(),
                request_id(&headers)?,
            )
            .await?,
    ))
}

async fn set_registry_lifecycle(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((resource, id)): Path<(String, uuid::Uuid)>,
    headers: HeaderMap,
    Json(input): Json<LifecycleInput>,
) -> Result<Json<ResourceMutationOutput>, AdminError> {
    let kind = resource.parse::<ResourceKind>()?;
    let source = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    Ok(Json(
        state
            .admin
            .set_registry_lifecycle(
                &context,
                ResourceTarget::new(kind, id),
                input,
                source,
                peer.ip(),
                request_id(&headers)?,
            )
            .await?,
    ))
}

async fn rotate_credential(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    Json(input): Json<RotateCredentialInput>,
) -> Result<(StatusCode, Json<ResourceMutationOutput>), AdminError> {
    let source = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    let output = state
        .admin
        .rotate_credential(
            &context,
            id,
            input,
            source,
            peer.ip(),
            request_id(&headers)?,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(output)))
}

#[derive(serde::Deserialize)]
struct ReplaceTokenInput {
    token: String,
}

fn uncertain_operation(error: impl std::fmt::Display) -> AdminError {
    tracing::error!(error = %error, "personal credential outcome could not be recorded");
    AdminError::OperationUncertain
}

async fn personal_api_key_status(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Path((provider, user_id)): Path<(String, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, AdminError> {
    let store = state
        .personal_tokens
        .as_ref()
        .ok_or(AdminError::NotFound)?
        .api_store(&provider);
    let guard = state
        .admin
        .lock_personal_subject(&context, user_id, Some(&provider))
        .await?;
    let fingerprint = store
        .get_with_lock(context.tenant_id, user_id)
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "personal API key status failed");
            AdminError::Unavailable
        })?
        .map(|(token, _)| crate::held_token::fingerprint(&token));
    guard.commit().await?;
    Ok(Json(
        serde_json::json!({"provider": provider, "configured": fingerprint.is_some(), "fingerprint": fingerprint}),
    ))
}

async fn personal_anthropic_status(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Path(user_id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, AdminError> {
    let store = state.personal_tokens.as_ref().ok_or(AdminError::NotFound)?;
    let guard = state
        .admin
        .lock_personal_subject(&context, user_id, None)
        .await?;
    let fingerprint = store
        .get_with_lock(context.tenant_id, user_id)
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "personal provider token status failed");
            AdminError::Unavailable
        })?
        .map(|(token, _)| crate::held_token::fingerprint(&token));
    guard.commit().await?;
    Ok(Json(
        serde_json::json!({"provider": "anthropic", "configured": fingerprint.is_some(), "fingerprint": fingerprint}),
    ))
}

async fn replace_personal_api_key(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((provider, user_id)): Path<(String, uuid::Uuid)>,
    headers: HeaderMap,
    Json(input): Json<ReplaceTokenInput>,
) -> Result<Json<serde_json::Value>, AdminError> {
    let store = state
        .personal_tokens
        .as_ref()
        .ok_or(AdminError::NotFound)?
        .api_store(&provider);
    let fingerprint = crate::held_token::validated_api_fingerprint(&input.token)
        .map_err(|_| AdminError::InvalidRequest)?;
    let addresses = (
        source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers),
        peer.ip(),
    );
    let id = request_id(&headers)?;
    state
        .admin
        .audit_personal_api_key(
            &context,
            (user_id, &provider),
            Some(&fingerprint),
            None,
            addresses,
            id,
        )
        .await?;
    let guard = state
        .admin
        .lock_personal_subject(&context, user_id, Some(&provider))
        .await?;
    let result = store
        .replace(context.tenant_id, user_id, &input.token)
        .await;
    guard.commit().await.map_err(uncertain_operation)?;
    if matches!(result, Err(crate::held_token::HeldTokenError::Uncertain)) {
        return Err(uncertain_operation("credential directory sync failed"));
    }
    state
        .admin
        .audit_personal_api_key(
            &context,
            (user_id, &provider),
            Some(&fingerprint),
            Some(result.is_ok()),
            addresses,
            id,
        )
        .await
        .map_err(uncertain_operation)?;
    result.map_err(|error| {
        tracing::error!(error = %error, "personal API key replacement failed");
        AdminError::Unavailable
    })?;
    Ok(Json(
        serde_json::json!({"provider": provider, "fingerprint": fingerprint}),
    ))
}

async fn revoke_personal_api_key(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((provider, user_id)): Path<(String, uuid::Uuid)>,
    headers: HeaderMap,
) -> Result<StatusCode, AdminError> {
    let store = state
        .personal_tokens
        .as_ref()
        .ok_or(AdminError::NotFound)?
        .api_store(&provider);
    let addresses = (
        source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers),
        peer.ip(),
    );
    let id = request_id(&headers)?;
    state
        .admin
        .audit_personal_api_key(&context, (user_id, &provider), None, None, addresses, id)
        .await?;
    let guard = state
        .admin
        .lock_personal_subject(&context, user_id, Some(&provider))
        .await?;
    let result = store.revoke(context.tenant_id, user_id).await;
    guard.commit().await.map_err(uncertain_operation)?;
    if matches!(result, Err(crate::held_token::HeldTokenError::Uncertain)) {
        return Err(uncertain_operation("credential directory sync failed"));
    }
    state
        .admin
        .audit_personal_api_key(
            &context,
            (user_id, &provider),
            None,
            Some(result.is_ok()),
            addresses,
            id,
        )
        .await
        .map_err(uncertain_operation)?;
    result.map_err(|error| {
        tracing::error!(error = %error, "personal API key revocation failed");
        AdminError::Unavailable
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn replace_personal_anthropic_token(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(user_id): Path<uuid::Uuid>,
    headers: HeaderMap,
    Json(input): Json<ReplaceTokenInput>,
) -> Result<Json<serde_json::Value>, AdminError> {
    let store = state.personal_tokens.as_ref().ok_or(AdminError::NotFound)?;
    let fingerprint = crate::held_token::validated_fingerprint(&input.token)
        .map_err(|_| AdminError::InvalidRequest)?;
    state
        .admin
        .audit_personal_token_attempt(
            &context,
            user_id,
            Some(&fingerprint),
            None,
            (
                source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers),
                peer.ip(),
            ),
            request_id(&headers)?,
        )
        .await?;
    let guard = state
        .admin
        .lock_personal_subject(&context, user_id, None)
        .await?;
    let result = store
        .replace(context.tenant_id, user_id, &input.token)
        .await;
    guard.commit().await.map_err(uncertain_operation)?;
    if matches!(result, Err(crate::held_token::HeldTokenError::Uncertain)) {
        return Err(uncertain_operation("credential directory sync failed"));
    }
    state
        .admin
        .audit_personal_token_attempt(
            &context,
            user_id,
            Some(&fingerprint),
            Some(result.is_ok()),
            (
                source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers),
                peer.ip(),
            ),
            request_id(&headers)?,
        )
        .await
        .map_err(uncertain_operation)?;
    result.map_err(|error| {
        tracing::error!(error = %error, "personal provider token replacement failed");
        AdminError::Unavailable
    })?;
    Ok(Json(
        serde_json::json!({"provider": "anthropic", "fingerprint": fingerprint}),
    ))
}

async fn revoke_personal_anthropic_token(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(user_id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode, AdminError> {
    let store = state.personal_tokens.as_ref().ok_or(AdminError::NotFound)?;
    state
        .admin
        .audit_personal_token_attempt(
            &context,
            user_id,
            None,
            None,
            (
                source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers),
                peer.ip(),
            ),
            request_id(&headers)?,
        )
        .await?;
    let guard = state
        .admin
        .lock_personal_subject(&context, user_id, None)
        .await?;
    let result = store.revoke(context.tenant_id, user_id).await;
    guard.commit().await.map_err(uncertain_operation)?;
    if matches!(result, Err(crate::held_token::HeldTokenError::Uncertain)) {
        return Err(uncertain_operation("credential directory sync failed"));
    }
    state
        .admin
        .audit_personal_token_attempt(
            &context,
            user_id,
            None,
            Some(result.is_ok()),
            (
                source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers),
                peer.ip(),
            ),
            request_id(&headers)?,
        )
        .await
        .map_err(uncertain_operation)?;
    result.map_err(|error| {
        tracing::error!(error = %error, "personal provider token revocation failed");
        AdminError::Unavailable
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn replace_anthropic_token(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<ReplaceTokenInput>,
) -> Result<Json<serde_json::Value>, AdminError> {
    let held = state.held_token.as_ref().ok_or(AdminError::NotFound)?;
    let fingerprint = held
        .replace(&input.token)
        .await
        .map_err(|error| match error {
            crate::held_token::HeldTokenError::Invalid => AdminError::InvalidRequest,
            crate::held_token::HeldTokenError::Storage(_) => AdminError::Unavailable,
            crate::held_token::HeldTokenError::Uncertain => AdminError::OperationUncertain,
        })?;
    let source = source_ip(&state.config.admin_trusted_proxy_ips, peer, &headers);
    state
        .admin
        .audit_provider_token_update(
            &context,
            &fingerprint,
            source,
            peer.ip(),
            request_id(&headers)?,
        )
        .await?;
    Ok(Json(
        serde_json::json!({"provider": "anthropic", "fingerprint": fingerprint}),
    ))
}

async fn require_admin_session(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Result<Response, AdminError> {
    let token = bearer(request.headers())?;
    let context = state.admin.resolve_session(token).await?;
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .ok_or(AdminError::Forbidden)?;
    let method = request.method().as_str();
    if authorize(method, route, context.role).is_none() {
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|value| value.0)
            .ok_or(AdminError::Unavailable)?;
        let source = source_ip(
            &state.config.admin_trusted_proxy_ips,
            peer,
            request.headers(),
        );
        state
            .admin
            .audit_denial(
                &context,
                method,
                route,
                source,
                peer.ip(),
                request_id(request.headers())?,
            )
            .await?;
        return Err(AdminError::Forbidden);
    }
    request.extensions_mut().insert(context);
    Ok(next.run(request).await)
}

fn bearer(headers: &HeaderMap) -> Result<&str, AdminError> {
    if headers.contains_key("x-api-key") {
        return Err(AdminError::Unauthorized);
    }
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .ok_or(AdminError::Unauthorized)
}

fn request_id(headers: &HeaderMap) -> Result<&str, AdminError> {
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.trim().is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| byte.is_ascii_graphic())
        })
        .ok_or(AdminError::InvalidRequest)
}

fn source_ip(
    trusted_proxies: &[std::net::IpAddr],
    peer: SocketAddr,
    headers: &HeaderMap,
) -> std::net::IpAddr {
    if !trusted_proxies.contains(&peer.ip()) {
        return peer.ip();
    }
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| peer.ip())
}

fn cors(origin: Option<&str>) -> CorsLayer {
    let layer = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static(REQUEST_ID_HEADER),
        ])
        .expose_headers([HeaderName::from_static(REQUEST_ID_HEADER)]);
    match origin.and_then(|value| HeaderValue::from_str(value).ok()) {
        Some(origin) => layer.allow_origin(origin),
        None => layer,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, SocketAddr},
        sync::Arc,
    };

    use axum::{
        Router,
        body::Body,
        extract::ConnectInfo,
        http::{Request, StatusCode},
        response::IntoResponse as _,
        routing::get,
    };
    use chrono::{Duration, SecondsFormat, Utc};
    use serde_json::json;
    use sqlx::{PgPool, Row as _};
    use tower::ServiceExt as _;
    use uuid::Uuid;

    use super::{bearer, cors, request_id, source_ip};
    use crate::{
        admin::{AdminAccountCreate, AdminRole, BootstrapInput},
        routes,
        state::AppState,
    };

    #[sqlx::test]
    async fn personal_token_http_is_scoped_and_audited(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let other_tenant = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let foreign_user = Uuid::new_v4();
        let bootstrap = "personal-http-bootstrap-token-at-least-32-bytes";
        let root = std::env::temp_dir().join(format!("personal-http-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut config = crate::routes::tests::test_config();
        config.admin_bootstrap_token = Some(bootstrap.to_owned());
        config.admin_bootstrap_tenant_id = Some(tenant_id);
        config.anthropic_personal_token_dir = Some(root.join("users"));
        config.anthropic_subscription_token_file = Some(root.join("legacy"));
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
        let peer: SocketAddr = "127.0.0.1:4567".parse().unwrap();
        let owner = state
            .admin
            .bootstrap(
                bootstrap,
                BootstrapInput {
                    display_name: "owner".to_owned(),
                    credential_name: "owner".to_owned(),
                },
                peer.ip(),
                peer.ip(),
                "bootstrap",
            )
            .await
            .unwrap();
        let session = state
            .admin
            .login(&owner.login_token, peer.ip(), peer.ip(), "login")
            .await
            .unwrap();
        sqlx::query("INSERT INTO users (id, tenant_id, display_name) VALUES ($1, $2, 'owner'), ($3, $4, 'other')")
            .bind(user_id).bind(tenant_id).bind(foreign_user).bind(other_tenant).execute(&pool).await.unwrap();
        let request = |method: &str, id: Uuid| {
            let mut request = Request::builder()
                .method(method)
                .uri(format!("/admin/v1/provider-tokens/anthropic/{id}"))
                .header("authorization", format!("Bearer {}", session.access_token))
                .header("content-type", "application/json")
                .header("x-request-id", Uuid::new_v4().to_string())
                .body(if method == "PUT" {
                    Body::from(r#"{"token":"sk-ant-oat01-owner"}"#)
                } else {
                    Body::empty()
                })
                .unwrap();
            request.extensions_mut().insert(ConnectInfo(peer));
            request
        };
        let app = routes::router(state.clone());
        assert_eq!(
            app.clone()
                .oneshot(request("PUT", foreign_user))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let response = app.clone().oneshot(request("PUT", user_id)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response_body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&response_body).contains("sk-ant-oat01-owner"));
        assert_eq!(
            &*state
                .personal_tokens
                .as_ref()
                .unwrap()
                .get(tenant_id, user_id)
                .await
                .unwrap()
                .unwrap(),
            "sk-ant-oat01-owner"
        );
        assert_eq!(
            app.oneshot(request("DELETE", user_id))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert!(
            state
                .personal_tokens
                .as_ref()
                .unwrap()
                .get(tenant_id, user_id)
                .await
                .unwrap()
                .is_none()
        );
        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM audit_logs WHERE target_id = $1 ORDER BY action",
        )
        .bind(user_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(actions.len(), 4);
        let audit_contents: Vec<String> = sqlx::query_scalar(
            "SELECT coalesce(after_state::text, '') FROM audit_logs WHERE target_id = $1",
        )
        .bind(user_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(
            audit_contents
                .iter()
                .all(|entry| !entry.contains("sk-ant-oat01-owner"))
        );
        assert!(
            actions
                .iter()
                .any(|value| value.ends_with("replace_success"))
        );
        assert!(
            actions
                .iter()
                .any(|value| value.ends_with("revoke_success"))
        );
        sqlx::query("INSERT INTO providers (id, tenant_id, provider_key, display_name, provider_type, base_url, credential_ref, supported_operations, lifecycle_state) VALUES ($1, $2, 'openai-primary', 'OpenAI', 'openai', 'https://example.invalid', 'env://OPENAI_API_KEY', ARRAY['openai_responses'], 'active')")
            .bind(Uuid::new_v4()).bind(tenant_id).execute(&pool).await.unwrap();
        let api_request = |method: &str, provider: &str| {
            let mut req = Request::builder()
                .method(method)
                .uri(format!("/admin/v1/provider-api-keys/{provider}/{user_id}"))
                .header("authorization", format!("Bearer {}", session.access_token))
                .header("content-type", "application/json")
                .header("x-request-id", Uuid::new_v4().to_string())
                .body(if method == "PUT" {
                    Body::from(r#"{"token":"owner-api-secret"}"#)
                } else {
                    Body::empty()
                })
                .unwrap();
            req.extensions_mut().insert(ConnectInfo(peer));
            req
        };
        let app = routes::router(state.clone());
        assert_eq!(
            app.clone()
                .oneshot(api_request("PUT", "unknown"))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let response = app
            .clone()
            .oneshot(api_request("PUT", "openai-primary"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("owner-api-secret"));
        assert_eq!(
            &*state
                .personal_tokens
                .as_ref()
                .unwrap()
                .api_store("openai-primary")
                .get(tenant_id, user_id)
                .await
                .unwrap()
                .unwrap(),
            "owner-api-secret"
        );
        assert_eq!(
            app.oneshot(api_request("DELETE", "openai-primary"))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert!(
            state
                .personal_tokens
                .as_ref()
                .unwrap()
                .api_store("openai-primary")
                .get(tenant_id, user_id)
                .await
                .unwrap()
                .is_none()
        );
        let api_audits: Vec<String> = sqlx::query_scalar("SELECT after_state::text FROM audit_logs WHERE target_id = $1 AND action LIKE 'admin.personal_api_key.%'")
            .bind(user_id).fetch_all(&pool).await.unwrap();
        assert_eq!(api_audits.len(), 4);
        assert!(
            api_audits
                .iter()
                .all(|entry| !entry.contains("owner-api-secret"))
        );
        sqlx::query("CREATE FUNCTION reject_personal_attempt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.request_id = 'attempt-failure' AND NEW.action = 'admin.personal_api_key.replace_attempt' THEN RAISE EXCEPTION 'injected attempt failure'; END IF; RETURN NEW; END $$")
            .execute(&pool).await.unwrap();
        sqlx::query("CREATE TRIGGER reject_personal_attempt BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_personal_attempt()")
            .execute(&pool).await.unwrap();
        let mut denied_request = Request::builder()
            .method("PUT")
            .uri(format!(
                "/admin/v1/provider-api-keys/openai-primary/{user_id}"
            ))
            .header("authorization", format!("Bearer {}", session.access_token))
            .header("content-type", "application/json")
            .header("x-request-id", "attempt-failure")
            .body(Body::from(r#"{"token":"attempt-failure-key"}"#))
            .unwrap();
        denied_request.extensions_mut().insert(ConnectInfo(peer));
        let denied = routes::router(state.clone())
            .oneshot(denied_request)
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            state
                .personal_tokens
                .as_ref()
                .unwrap()
                .api_store("openai-primary")
                .get(tenant_id, user_id)
                .await
                .unwrap()
                .is_none()
        );
        sqlx::query("CREATE FUNCTION reject_personal_result() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.request_id = 'audit-failure' AND NEW.action = 'admin.personal_api_key.replace_success' THEN RAISE EXCEPTION 'injected audit failure'; END IF; RETURN NEW; END $$")
            .execute(&pool).await.unwrap();
        sqlx::query("CREATE TRIGGER reject_personal_result BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_personal_result()")
            .execute(&pool).await.unwrap();
        let mut failure_request = Request::builder()
            .method("PUT")
            .uri(format!(
                "/admin/v1/provider-api-keys/openai-primary/{user_id}"
            ))
            .header("authorization", format!("Bearer {}", session.access_token))
            .header("content-type", "application/json")
            .header("x-request-id", "audit-failure")
            .body(Body::from(r#"{"token":"audit-failure-key"}"#))
            .unwrap();
        failure_request.extensions_mut().insert(ConnectInfo(peer));
        let response = routes::router(state.clone())
            .oneshot(failure_request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let failure_body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let failure: serde_json::Value = serde_json::from_slice(&failure_body).unwrap();
        assert_eq!(failure["error"]["code"], "admin_operation_uncertain");
        let api_store = state
            .personal_tokens
            .as_ref()
            .unwrap()
            .api_store("openai-primary");
        assert_eq!(
            &*api_store.get(tenant_id, user_id).await.unwrap().unwrap(),
            "audit-failure-key"
        );
        let status = routes::router(state.clone())
            .oneshot(api_request("GET", "openai-primary"))
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let status_body = axum::body::to_bytes(status.into_body(), 4096)
            .await
            .unwrap();
        let status_body: serde_json::Value = serde_json::from_slice(&status_body).unwrap();
        assert_eq!(status_body["configured"], true);
        assert_eq!(
            status_body["fingerprint"],
            crate::held_token::fingerprint("audit-failure-key")
        );
        assert!(!status_body.to_string().contains("audit-failure-key"));
        let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE request_id = 'audit-failure' AND action = 'admin.personal_api_key.replace_attempt'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(attempts, 1);
        api_store.revoke(tenant_id, user_id).await.unwrap();
        for (method, request_id) in [("PUT", "sync-put"), ("DELETE", "sync-delete")] {
            state.personal_tokens.as_ref().unwrap().fail_next_sync();
            let mut request = Request::builder()
                .method(method)
                .uri(format!(
                    "/admin/v1/provider-api-keys/openai-primary/{user_id}"
                ))
                .header("authorization", format!("Bearer {}", session.access_token))
                .header("content-type", "application/json")
                .header("x-request-id", request_id)
                .body(if method == "PUT" {
                    Body::from(r#"{"token":"sync-test-key"}"#)
                } else {
                    Body::empty()
                })
                .unwrap();
            request.extensions_mut().insert(ConnectInfo(peer));
            let response = routes::router(state.clone())
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["error"]["code"], "admin_operation_uncertain");
            assert_eq!(
                api_store.get(tenant_id, user_id).await.unwrap().is_some(),
                method == "PUT"
            );
            let actions: Vec<String> =
                sqlx::query_scalar("SELECT action FROM audit_logs WHERE request_id = $1")
                    .bind(request_id)
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            assert_eq!(actions.len(), 1);
            assert!(actions[0].ends_with("_attempt"));
        }
        for (method, request_id) in [("PUT", "token-sync-put"), ("DELETE", "token-sync-delete")] {
            state.personal_tokens.as_ref().unwrap().fail_next_sync();
            let mut request = Request::builder()
                .method(method)
                .uri(format!("/admin/v1/provider-tokens/anthropic/{user_id}"))
                .header("authorization", format!("Bearer {}", session.access_token))
                .header("content-type", "application/json")
                .header("x-request-id", request_id)
                .body(if method == "PUT" {
                    Body::from(r#"{"token":"sk-ant-oat01-sync-test"}"#)
                } else {
                    Body::empty()
                })
                .unwrap();
            request.extensions_mut().insert(ConnectInfo(peer));
            let response = routes::router(state.clone())
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["error"]["code"], "admin_operation_uncertain");
            assert_eq!(
                state
                    .personal_tokens
                    .as_ref()
                    .unwrap()
                    .get(tenant_id, user_id)
                    .await
                    .unwrap()
                    .is_some(),
                method == "PUT"
            );
            let actions: Vec<String> =
                sqlx::query_scalar("SELECT action FROM audit_logs WHERE request_id = $1")
                    .bind(request_id)
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            assert_eq!(actions.len(), 1);
            assert!(actions[0].ends_with("_attempt"));
        }
        sqlx::query("CREATE FUNCTION notify_personal_attempt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.request_id = 'concurrent-personal' THEN PERFORM pg_notify('personal_attempt', 'ready'); END IF; RETURN NEW; END $$")
            .execute(&pool).await.unwrap();
        sqlx::query("CREATE TRIGGER notify_personal_attempt AFTER INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION notify_personal_attempt()")
            .execute(&pool).await.unwrap();
        let mut listener = sqlx::postgres::PgListener::connect_with(&pool)
            .await
            .unwrap();
        listener.listen("personal_attempt").await.unwrap();
        let mut disabled = pool.begin().await.unwrap();
        sqlx::query("UPDATE users SET status = 'disabled' WHERE tenant_id = $1 AND id = $2")
            .bind(tenant_id)
            .bind(user_id)
            .execute(&mut *disabled)
            .await
            .unwrap();
        let mut concurrent_request = Request::builder()
            .method("PUT")
            .uri(format!(
                "/admin/v1/provider-api-keys/openai-primary/{user_id}"
            ))
            .header("authorization", format!("Bearer {}", session.access_token))
            .header("content-type", "application/json")
            .header("x-request-id", "concurrent-personal")
            .body(Body::from(r#"{"token":"must-not-save"}"#))
            .unwrap();
        concurrent_request
            .extensions_mut()
            .insert(ConnectInfo(peer));
        let concurrent_app = routes::router(state.clone());
        let pending =
            tokio::spawn(async move { concurrent_app.oneshot(concurrent_request).await.unwrap() });
        tokio::time::timeout(std::time::Duration::from_secs(2), listener.recv())
            .await
            .unwrap()
            .unwrap();
        disabled.commit().await.unwrap();
        let blocked = pending.await.unwrap();
        assert_eq!(blocked.status(), StatusCode::NOT_FOUND);
        assert!(
            state
                .personal_tokens
                .as_ref()
                .unwrap()
                .api_store("openai-primary")
                .get(tenant_id, user_id)
                .await
                .unwrap()
                .is_none()
        );
        sqlx::query("UPDATE users SET status = 'active' WHERE tenant_id = $1 AND id = $2")
            .bind(tenant_id)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        let context = state
            .admin
            .resolve_session(&session.access_token)
            .await
            .unwrap();
        let subject_lock = state
            .admin
            .lock_personal_subject(&context, user_id, Some("openai-primary"))
            .await
            .unwrap();
        let mut changing = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '100ms'")
            .execute(&mut *changing)
            .await
            .unwrap();
        let blocked =
            sqlx::query("UPDATE users SET status = 'disabled' WHERE tenant_id = $1 AND id = $2")
                .bind(tenant_id)
                .bind(user_id)
                .execute(&mut *changing)
                .await;
        assert!(blocked.is_err());
        changing.rollback().await.unwrap();
        subject_lock.rollback().await.unwrap();
        state
            .admin
            .audit_personal_api_key(
                &context,
                (user_id, "openai-primary"),
                Some("fingerprint"),
                None,
                (peer.ip(), peer.ip()),
                "race-attempt",
            )
            .await
            .unwrap();
        sqlx::query("UPDATE users SET status = 'disabled' WHERE tenant_id = $1 AND id = $2")
            .bind(tenant_id)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        state
            .admin
            .audit_personal_api_key(
                &context,
                (user_id, "openai-primary"),
                Some("fingerprint"),
                Some(true),
                (peer.ip(), peer.ip()),
                "race-result",
            )
            .await
            .unwrap();
        let result_count: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE request_id = 'race-result' AND action = 'admin.personal_api_key.replace_success'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(result_count, 1);
    }

    #[test]
    fn admin_headers_reject_data_plane_credentials_and_invalid_request_ids() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-api-key", "client-key".parse().unwrap());
        headers.insert("authorization", "Bearer admin-token".parse().unwrap());
        assert_eq!(bearer(&headers).unwrap_err().into_response().status(), 401);

        headers.remove("x-api-key");
        assert_eq!(bearer(&headers).unwrap(), "admin-token");
        headers.insert("x-request-id", "contains space".parse().unwrap());
        assert_eq!(
            request_id(&headers).unwrap_err().into_response().status(),
            400
        );
    }

    #[test]
    fn forwarded_source_is_accepted_only_from_an_exact_trusted_peer() {
        let trusted = "10.0.0.2".parse().unwrap();
        let untrusted: SocketAddr = "10.0.0.3:1234".parse().unwrap();
        let trusted_peer: SocketAddr = "10.0.0.2:1234".parse().unwrap();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.9, 10.0.0.2".parse().unwrap());
        assert_eq!(source_ip(&[trusted], untrusted, &headers), untrusted.ip());
        assert_eq!(
            source_ip(&[trusted], trusted_peer, &headers),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn cors_allows_only_the_configured_origin() {
        let app = Router::new()
            .route("/admin/v1/test", get(|| async { "ok" }))
            .layer(cors(Some("https://admin.example.com")));
        let allowed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin/v1/test")
                    .header("origin", "https://admin.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            allowed
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "https://admin.example.com"
        );

        let denied = app
            .oneshot(
                Request::builder()
                    .uri("/admin/v1/test")
                    .header("origin", "https://attacker.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            denied.headers().get("access-control-allow-origin").unwrap(),
            "https://attacker.example.com"
        );
    }

    #[sqlx::test]
    async fn viewer_mutation_is_forbidden_and_audited_while_operator_is_allowed(pool: PgPool) {
        let tenant_id = Uuid::new_v4();
        let bootstrap_token = "admin-router-bootstrap-token-at-least-32-bytes";
        let mut config = crate::routes::tests::test_config();
        config.database_max_connections = 4;
        config.admin_database_max_connections = 2;
        config.admin_bootstrap_token = Some(bootstrap_token.to_owned());
        config.admin_bootstrap_tenant_id = Some(tenant_id);
        let state = Arc::new(AppState::new(config, pool.clone()).unwrap());
        let peer: SocketAddr = "127.0.0.1:4567".parse().unwrap();
        let owner = state
            .admin
            .bootstrap(
                bootstrap_token,
                BootstrapInput {
                    display_name: "owner".to_owned(),
                    credential_name: "owner login".to_owned(),
                },
                peer.ip(),
                peer.ip(),
                "bootstrap",
            )
            .await
            .unwrap();
        let owner_session = state
            .admin
            .login(&owner.login_token, peer.ip(), peer.ip(), "owner-login")
            .await
            .unwrap();
        let owner_context = state
            .admin
            .resolve_session(&owner_session.access_token)
            .await
            .unwrap();
        let viewer = state
            .admin
            .create_admin_account(
                &owner_context,
                AdminAccountCreate {
                    display_name: "viewer".to_owned(),
                    credential_name: "viewer login".to_owned(),
                    role: AdminRole::Viewer,
                },
                peer.ip(),
                peer.ip(),
                "create-viewer",
            )
            .await
            .unwrap();
        let operator = state
            .admin
            .create_admin_account(
                &owner_context,
                AdminAccountCreate {
                    display_name: "operator".to_owned(),
                    credential_name: "operator login".to_owned(),
                    role: AdminRole::Operator,
                },
                peer.ip(),
                peer.ip(),
                "create-operator",
            )
            .await
            .unwrap();
        let viewer_session = state
            .admin
            .login(&viewer.login_token, peer.ip(), peer.ip(), "viewer-login")
            .await
            .unwrap();
        let operator_session = state
            .admin
            .login(
                &operator.login_token,
                peer.ip(),
                peer.ip(),
                "operator-login",
            )
            .await
            .unwrap();

        let from = (Utc::now() - Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let to = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let viewer_read = Request::builder()
            .uri(format!("/admin/v1/overview?from={from}&to={to}"))
            .header(
                "authorization",
                format!("Bearer {}", viewer_session.access_token),
            )
            .body(Body::empty())
            .unwrap();
        let readable = routes::router(state.clone())
            .oneshot(viewer_read)
            .await
            .unwrap();
        assert_eq!(readable.status(), StatusCode::OK);

        let mut viewer_request = Request::builder()
            .method("POST")
            .uri("/admin/v1/users")
            .header(
                "authorization",
                format!("Bearer {}", viewer_session.access_token),
            )
            .header("content-type", "application/json")
            .header("x-request-id", "viewer-denied")
            .body(Body::from(json!({"display_name": "blocked"}).to_string()))
            .unwrap();
        viewer_request.extensions_mut().insert(ConnectInfo(peer));
        let denied = routes::router(state.clone())
            .oneshot(viewer_request)
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let denial = sqlx::query(
            r#"
            SELECT actor_role, action, request_id
            FROM audit_logs
            WHERE tenant_id = $1 AND action = 'admin.authorization.denied'
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(denial.get::<String, _>("actor_role"), "viewer");
        assert_eq!(denial.get::<String, _>("request_id"), "viewer-denied");

        let mut operator_request = Request::builder()
            .method("POST")
            .uri("/admin/v1/users")
            .header(
                "authorization",
                format!("Bearer {}", operator_session.access_token),
            )
            .header("content-type", "application/json")
            .header("x-request-id", "operator-allowed")
            .body(Body::from(json!({"display_name": "allowed"}).to_string()))
            .unwrap();
        operator_request.extensions_mut().insert(ConnectInfo(peer));
        let allowed = routes::router(state.clone())
            .oneshot(operator_request)
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::CREATED);

        let mut oversized_request = Request::builder()
            .method("POST")
            .uri("/admin/v1/users")
            .header(
                "authorization",
                format!("Bearer {}", operator_session.access_token),
            )
            .header("content-type", "application/json")
            .header("x-request-id", "oversized-admin-body")
            .body(Body::from(vec![b'x'; 1024 * 1024 + 1]))
            .unwrap();
        oversized_request.extensions_mut().insert(ConnectInfo(peer));
        let oversized = routes::router(state)
            .oneshot(oversized_request)
            .await
            .unwrap();
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
