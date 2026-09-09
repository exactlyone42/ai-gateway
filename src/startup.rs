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
    // `shutdown()` blocks until the exporter drains or times out, so it runs
    // off the async worker: a wedged backend would otherwise hold a worker
    // thread for the whole timeout.
    if let Some(traces) = traces {
        let flushed = tokio::task::spawn_blocking(move || traces.shutdown()).await;
        match flushed {
            Ok(Err(error)) => tracing::error!(
                error.type = "otlp_shutdown_failed",
                exception.message = %error,
                "buffered spans may be lost"
            ),
            Err(error) => tracing::error!(
                error.type = "otlp_shutdown_panicked",
                exception.message = %error,
                "buffered spans may be lost"
            ),
            Ok(Ok(())) => {}
        }
    }
    Ok(())
}

/// Waits for whichever stop signal arrives first.
///
/// SIGTERM is the one that matters: containers and service managers send it,
/// and without it the process is killed outright -- the trace flush above
/// never runs. Ctrl-C stays for the terminal.
///
/// Boundary: this only starts the shutdown. Bounded draining of in-flight
/// streams is W19 (`loops/process-lifecycle.loop.md`).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => {
                tracing::error!(
                    error.type = "signal_handler_unavailable",
                    exception.message = %error,
                    "SIGTERM is not handled; only Ctrl-C stops this process cleanly"
                );
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
