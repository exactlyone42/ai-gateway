use std::{
    future::Future,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum_server::Handle;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::{config::Config, db, logging, metrics, model_sync, routes, state::AppState};

/// Brings the process up: environment, logging, configuration, then serves
/// until a shutdown signal arrives.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let config = Config::from_env()?;
    let traces = logging::init(config.otlp_endpoint.as_deref());
    let meters = metrics::init(config.otlp_endpoint.as_deref());
    if config.credential_environment == "dev"
        && !config.allowed_models.iter().any(|model| {
            let model = model.to_ascii_lowercase();
            model.contains("claude") || model.contains("anthropic")
        })
    {
        tracing::warn!(
            error.type = "anthropic_model_not_discoverable",
            "development model ids contain neither claude nor anthropic; Claude Code discovery may show no models"
        );
    }
    let address = config.listen_address;

    let pool = db::pool(&config.database_url, config.data_database_max_connections())?;
    db::migrate(&pool).await?;
    info!("database schema is up to date");

    let state = Arc::new(AppState::new(config, pool)?);
    if let Err(error) = model_sync::refresh(&state.pool, &state.client).await {
        tracing::warn!(event.name = "model_catalog.refresh_failed", exception.message = %error, "startup model catalog refresh failed");
    }
    let model_sync = model_sync::spawn(state.pool.clone(), state.client.clone());
    initialize_concurrency_guard(&state.quotas, &state.config.scheduler).await?;
    let recovered = state.ledger.flush_startup().await?;
    if recovered > 0 {
        info!(
            ledger.envelopes = recovered,
            "recovered usage from the durable spool"
        );
    }

    let listener = std::net::TcpListener::bind(address)?;
    listener.set_nonblocking(true)?;
    info!(%address, "agent pool gateway started");
    let quota_guard = state.quotas.clone();
    let guard_lost = Arc::new(AtomicBool::new(false));
    let shutdown_guard_lost = Arc::clone(&guard_lost);
    let shutdown = async move {
        tokio::select! {
            _ = shutdown_signal() => {}
            _ = quota_guard.wait_for_single_instance_guard_loss() => {
                shutdown_guard_lost.store(true, Ordering::Release);
                tracing::error!(error.type = "single_instance_guard_lost", "single instance guard is unavailable");
            }
        }
    };
    serve_with_shutdown(listener, Arc::clone(&state), shutdown).await?;
    model_sync.abort();

    report_flush_bounded("spans", state.config.shutdown_finalize_timeout, move || {
        traces.map_or(Ok(()), |provider| provider.shutdown())
    })
    .await;
    report_flush_bounded(
        "metrics",
        state.config.shutdown_finalize_timeout,
        move || meters.map_or(Ok(()), |provider| provider.shutdown()),
    )
    .await;
    if guard_lost.load(Ordering::Acquire) {
        return Err("single instance guard lost".into());
    }
    Ok(())
}

pub(crate) async fn serve_with_shutdown<F>(
    listener: std::net::TcpListener,
    state: Arc<AppState>,
    signal: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let handle = Handle::<SocketAddr>::new();
    let server_done = CancellationToken::new();
    let controller = tokio::spawn(drain_on_signal(
        Arc::clone(&state),
        handle.clone(),
        server_done.clone(),
        signal,
    ));
    let mut server = axum_server::from_tcp(listener)?.handle(handle);
    server
        .http_builder()
        .http1()
        .max_buf_size(state.config.transport_header_max_bytes());
    let result = server
        .serve(
            routes::router(Arc::clone(&state)).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    server_done.cancel();
    if result.is_err() {
        controller.abort();
    } else if let Err(error) = controller.await {
        tracing::error!(
            error.type = "shutdown_controller_failed",
            exception.message = %error,
            %address,
            "shutdown controller did not finish cleanly"
        );
    }
    bounded_ledger_shutdown(&state).await;
    result
}

async fn drain_on_signal<F>(
    state: Arc<AppState>,
    handle: Handle<SocketAddr>,
    server_done: CancellationToken,
    signal: F,
) where
    F: Future<Output = ()>,
{
    signal.await;
    let active = state.lifecycle.begin_draining();
    if let Err(error) = state.scheduler.close() {
        tracing::error!(
            error.type = "scheduler_close_failed",
            exception.message = %error,
            "queued requests could not be released during shutdown"
        );
    }
    info!(
        process.active_requests = active,
        process.drain_timeout_ms = state.config.drain_timeout.as_millis() as u64,
        "gateway is draining"
    );

    let deadline = Instant::now() + state.config.drain_timeout;
    match timeout_at(deadline, state.lifecycle.wait_for_idle()).await {
        Ok(()) => {
            handle.graceful_shutdown(None);
            tokio::select! {
                _ = server_done.cancelled() => {}
                _ = sleep_until(deadline) => {
                    if handle.connection_count() > 0 {
                        state.lifecycle.force();
                        tracing::warn!(
                            error.type = "shutdown_timeout",
                            process.active_connections = handle.connection_count(),
                            "drain deadline elapsed while flushing connections"
                        );
                        handle.shutdown();
                    }
                }
            }
        }
        Err(_) => {
            state.lifecycle.force();
            tracing::warn!(
                error.type = "shutdown_timeout",
                process.active_requests = state.lifecycle.active(),
                "drain deadline elapsed; active connections are being closed"
            );
            handle.shutdown();
        }
    }
}

async fn bounded_ledger_shutdown(state: &AppState) {
    let deadline = Instant::now() + state.config.shutdown_finalize_timeout;
    if timeout_at(deadline, state.lifecycle.wait_for_idle())
        .await
        .is_err()
    {
        tracing::error!(
            error.type = "request_shutdown_timeout",
            process.active_requests = state.lifecycle.active(),
            "active response bodies remain for startup recovery"
        );
        return;
    }
    if timeout_at(deadline, state.ledger.wait_for_finalizations())
        .await
        .is_err()
    {
        tracing::error!(
            error.type = "ledger_shutdown_timeout",
            ledger.pending_finalizations = state.ledger.pending_finalizations(),
            "interrupted request finalization remains for startup recovery"
        );
        return;
    }
    if timeout_at(deadline, state.ledger.wait_for_idle())
        .await
        .is_err()
    {
        tracing::error!(
            error.type = "ledger_flush_shutdown_timeout",
            "usage remains in the durable spool for startup recovery"
        );
    }
}

async fn initialize_concurrency_guard(
    quotas: &crate::quota::QuotaEngine,
    scheduler: &crate::scheduler::SchedulerConfig,
) -> Result<(), crate::quota::QuotaError> {
    if !scheduler.passthrough {
        quotas.acquire_single_instance_guard().await?;
    }
    Ok(())
}

async fn report_flush_bounded<E, F>(signal: &'static str, limit: Duration, flush: F)
where
    E: std::fmt::Display + Send + 'static,
    F: FnOnce() -> Result<(), E> + Send + 'static,
{
    match timeout(limit, tokio::task::spawn_blocking(flush)).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => tracing::error!(
            error.type = "otlp_shutdown_failed",
            otel.signal = signal,
            exception.message = %error,
            "buffered telemetry may be lost"
        ),
        Ok(Err(error)) => tracing::error!(
            error.type = "otlp_shutdown_panicked",
            otel.signal = signal,
            exception.message = %error,
            "buffered telemetry may be lost"
        ),
        Err(_) => tracing::error!(
            error.type = "otlp_shutdown_timeout",
            otel.signal = signal,
            "buffered telemetry flush exceeded the shutdown deadline"
        ),
    }
}

/// Waits for whichever stop signal arrives first.
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

#[cfg(test)]
mod tests {
    use super::initialize_concurrency_guard;
    use crate::{
        quota::{QuotaEngine, QuotaError},
        scheduler::SchedulerConfig,
    };
    use sqlx::PgPool;

    #[sqlx::test]
    async fn startup_keeps_single_owner_when_scheduling_is_enabled(pool: PgPool) {
        let first = QuotaEngine::new(pool.clone());
        let second = QuotaEngine::new(pool);
        let scheduler = SchedulerConfig::default();
        initialize_concurrency_guard(&first, &scheduler)
            .await
            .unwrap();
        assert!(matches!(
            initialize_concurrency_guard(&second, &scheduler).await,
            Err(QuotaError::MultipleInstances)
        ));
    }

    #[sqlx::test]
    async fn passthrough_startup_can_overlap_a_legacy_guard_holder(pool: PgPool) {
        let legacy = QuotaEngine::new(pool.clone());
        legacy.acquire_single_instance_guard().await.unwrap();
        let first = QuotaEngine::new(pool.clone());
        let second = QuotaEngine::new(pool);
        let scheduler = SchedulerConfig {
            passthrough: true,
            ..SchedulerConfig::default()
        };
        initialize_concurrency_guard(&first, &scheduler)
            .await
            .unwrap();
        initialize_concurrency_guard(&second, &scheduler)
            .await
            .unwrap();
        assert!(matches!(
            initialize_concurrency_guard(&second, &SchedulerConfig::default()).await,
            Err(QuotaError::MultipleInstances)
        ));
    }
}
