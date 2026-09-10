use std::sync::Arc;

use tracing::info;

use crate::{config::Config, db, logging, metrics, routes, state::AppState};

/// Brings the process up: environment, logging, configuration, then serves
/// until a shutdown signal arrives.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let config = Config::from_env()?;
    let traces = logging::init(config.otlp_endpoint.as_deref());
    let meters = metrics::init(config.otlp_endpoint.as_deref());
    if !config.allowed_models.iter().any(|model| {
        let model = model.to_ascii_lowercase();
        model.contains("claude") || model.contains("anthropic")
    }) {
        tracing::warn!(
            error.type = "anthropic_model_not_discoverable",
            "configured model ids contain neither claude nor anthropic; Claude Code discovery may show no models"
        );
    }
    let address = config.listen_address;

    // Migrations run before the listener binds. A gateway serving requests
    // against a half-migrated schema is worse than one that did not start, so
    // this failure is fatal and says so.
    let pool = db::pool(&config.database_url, config.data_database_max_connections())?;
    db::migrate(&pool).await?;
    info!("database schema is up to date");

    let state = Arc::new(AppState::new(config, pool)?);
    let recovered = state.ledger.flush_startup().await?;
    if recovered > 0 {
        info!(
            ledger.envelopes = recovered,
            "recovered usage from the durable spool"
        );
    }

    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(%address, "agent pool gateway started");
    axum::serve(
        listener,
        routes::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    // Spans buffered by the batch exporter are lost unless the provider is
    // shut down, so a clean exit is the difference between having the last
    // requests in the trace backend and not.
    // `shutdown()` blocks until the exporter drains or times out, so it runs
    // off the async worker: a wedged backend would otherwise hold a worker
    // thread for the whole timeout.
    if let Some(traces) = traces {
        report_flush(
            "spans",
            tokio::task::spawn_blocking(move || traces.shutdown()).await,
        );
    }
    if let Some(meters) = meters {
        report_flush(
            "metrics",
            tokio::task::spawn_blocking(move || meters.shutdown()).await,
        );
    }
    Ok(())
}

/// Says what was lost, and never hides a failed flush.
fn report_flush<E: std::fmt::Display>(
    signal: &str,
    outcome: Result<Result<(), E>, tokio::task::JoinError>,
) {
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!(
            error.type = "otlp_shutdown_failed",
            otel.signal = signal,
            exception.message = %error,
            "buffered telemetry may be lost"
        ),
        Err(error) => tracing::error!(
            error.type = "otlp_shutdown_panicked",
            otel.signal = signal,
            exception.message = %error,
            "buffered telemetry may be lost"
        ),
    }
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
