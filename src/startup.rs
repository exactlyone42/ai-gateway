use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};

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
    validate_capture_startup(&config)?;
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
    serve_with_shutdown(listener, Arc::clone(&state), shutdown_signal()).await?;
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
    Ok(())
}

pub async fn initialize_content_capture() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let config = Config::from_env()?;
    crate::capture_paths::validate_paths([
        &config.content_capture_spool_dir,
        &config.content_capture_key_dir,
        &config.content_capture_anchor_dir,
    ])?;
    let pool = db::pool(&config.database_url, 1)?;
    let mut tx = pool.begin().await?;
    let (records, usage): (bool, bool) = sqlx::query_as("SELECT to_regclass('public.capture_records') IS NOT NULL, to_regclass('public.capture_master_key_usage') IS NOT NULL")
        .fetch_one(&mut *tx).await?;
    if records != usage {
        return Err("content storage database schema is incomplete".into());
    }
    if records {
        sqlx::query(
            "LOCK TABLE capture_records, capture_master_key_usage IN ACCESS EXCLUSIVE MODE NOWAIT",
        )
        .execute(&mut *tx)
        .await?;
        let count: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM capture_records) + (SELECT count(*) FROM capture_master_key_usage)")
            .fetch_one(&mut *tx).await?;
        if count != 0 {
            return Err("content storage database already has records".into());
        }
    }
    let others: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND backend_type='client backend' AND pid<>pg_backend_pid()")
        .fetch_one(&mut *tx).await?;
    if others != 0 {
        return Err("content storage initialization requires an isolated database".into());
    }
    let keys = config.content_capture_key_dir;
    let anchors = config.content_capture_anchor_dir;
    let spool = config.content_capture_spool_dir;
    let fingerprint = tokio::task::spawn_blocking(move || {
        crate::capture_keyring::initialize(&keys, &anchors, &spool)
    })
    .await??;
    tx.commit().await?;
    println!(
        "content storage initialized: {}",
        fingerprint
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    Ok(())
}

fn validate_capture_startup(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    validate_capture_storage(
        config.content_capture_enabled,
        &config.content_capture_spool_dir,
        &config.content_capture_key_dir,
        &config.content_capture_anchor_dir,
    )
}

fn validate_capture_storage(
    enabled: bool,
    spool: &std::path::Path,
    keys: &std::path::Path,
    anchors: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !enabled {
        return Ok(());
    }
    crate::capture_paths::validate_paths([spool, keys, anchors])?;
    let (master, _guard) = crate::capture_keyring::locked_current(keys)?;
    crate::capture_anchor::check_current_anchor(anchors, &master)?;
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
    use super::{initialize_concurrency_guard, validate_capture_storage};
    use crate::{
        quota::{QuotaEngine, QuotaError},
        scheduler::SchedulerConfig,
    };
    use sqlx::PgPool;

    #[test]
    fn capture_startup_requires_safe_directories_and_valid_key_and_counter() {
        use std::os::unix::fs::PermissionsExt;
        let base =
            std::path::Path::new("/tmp").join(format!("capture-startup-{}", uuid::Uuid::new_v4()));
        let spool = base.join("spool");
        let keys = base.join("keys");
        let anchors = base.join("anchors");
        for path in [&base, &spool, &keys, &anchors] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(validate_capture_storage(false, &spool, &keys, &anchors).is_ok());
        assert!(validate_capture_storage(true, &spool, &keys, &anchors).is_err());
        let fingerprint = crate::capture_keyring::rotate(&keys, &anchors, &spool).unwrap();
        assert!(validate_capture_storage(true, &spool, &keys, &anchors).is_ok());
        let name: String = fingerprint
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(anchors.join(format!("{name}.anchor")))
            .unwrap();
        lock.lock().unwrap();
        assert!(validate_capture_storage(true, &spool, &keys, &anchors).is_err());
        drop(lock);
        assert!(validate_capture_storage(true, &spool, &keys, &anchors).is_ok());
        std::fs::write(anchors.join(format!("{name}.anchor")), [0u8; 81]).unwrap();
        assert!(validate_capture_storage(true, &spool, &keys, &anchors).is_err());
        assert!(validate_capture_storage(false, &spool, &keys, &anchors).is_ok());
    }

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
