use std::sync::Arc;

use tracing::info;

use crate::{config::Config, logging, routes, state::AppState};

/// Brings the process up: environment, logging, configuration, then serves
/// until a shutdown signal arrives.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let config = Config::from_env()?;
    let traces = logging::init(config.otlp_endpoint.as_deref());
    let address = config.listen_address;
    let state = Arc::new(AppState::new(config));

    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(%address, "agent pool gateway started");
    axum::serve(listener, routes::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // Spans buffered by the batch exporter are lost unless the provider is
    // shut down, so a clean exit is the difference between having the last
    // requests in the trace backend and not.
    if let Some(traces) = traces
        && let Err(error) = traces.shutdown()
    {
        tracing::error!(
            error.type = "otlp_shutdown_failed",
            exception.message = %error,
            "buffered spans may be lost"
        );
    }
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
