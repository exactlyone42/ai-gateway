mod config;
mod error;
mod gateway;

use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};
use config::Config;
use gateway::{AppState, chat_completions, health};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
        .init();

    let config = Config::from_env()?;
    let address = config.listen_address;
    let state = Arc::new(AppState::new(config));
    let request_id_header = axum::http::HeaderName::from_static("x-request-id");

    let app = Router::new()
        .route("/healthz", get(health))
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(state)
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(%address, "agent pool gateway started");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
