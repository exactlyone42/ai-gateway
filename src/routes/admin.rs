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

use crate::prelude::*;
use crate::{
    admin::{
        AdminAccountCreate, AdminAccountPatch, AdminAccountSecretOutput, AdminContext, AdminError,
        BootstrapInput, BootstrapOutput, CursorPage, DeleteQuery, LifecycleInput, ListQuery,
        ReportListQuery, ReportWindowQuery, ResourceKind, ResourceMutationOutput, ResourcePage,
        ResourceTarget, RotateCredentialInput, SessionOutput, UsageQuery, authorize,
    },
    middleware::REQUEST_ID_HEADER,
};

pub fn router(state: &Arc<AppState>) -> Router<Arc<AppState>> {
    let protected = Router::new()
        .route("/admin/v1/session", delete(logout))
        .route("/admin/v1/overview", get(overview))
        .route("/admin/v1/identity-map", get(identity_map))
        .route("/admin/v1/requests", get(request_page))
        .route("/admin/v1/requests/{id}", get(request_report))
        .route("/admin/v1/usage", get(usage_report))
        .route("/admin/v1/audit", get(audit_page))
        .route("/admin/v1/catalog", get(catalog_report))
        .route("/admin/v1/governance", get(governance_report))
        .route("/admin/v1/system-health", get(system_health_report))
        .route("/admin/v1/credentials/{id}/rotate", post(rotate_credential))
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

async fn overview(
    State(state): State<Arc<AppState>>,
    Extension(context): Extension<AdminContext>,
    Query(query): Query<ReportWindowQuery>,
) -> Result<Json<serde_json::Value>, AdminError> {
    Ok(Json(state.admin.overview_report(&context, query).await?))
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
    Query(query): Query<ReportListQuery>,
) -> Result<Json<CursorPage>, AdminError> {
    Ok(Json(state.admin.request_page(&context, query).await?))
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
        .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
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
