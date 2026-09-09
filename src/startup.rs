use std::sync::Arc;

use tracing::info;

use crate::{config::Config, logging, routes, state::AppState};

/// Brings the process up: environment, logging, configuration, then serves
/// until a shutdown signal arrives.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    logging::init();

    let config = Config::from_env()?;
    let address = config.listen_address;
    let state = Arc::new(AppState::new(config));

    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(%address, "agent pool gateway started");
    axum::serve(listener, routes::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
