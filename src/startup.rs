use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};

use axum_server::Handle;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::{config::Config, db, logging, metrics, model_sync, routes, state::AppState};

fn erasure_floor_error(error: std::io::Error) -> std::io::Error {
    tracing::error!(error.type = "capture_erasure_floor_invalid", error.kind = ?error.kind(), "capture erasure position check failed");
    let message = match error.kind() {
        std::io::ErrorKind::WouldBlock => "capture erasure journal busy",
        std::io::ErrorKind::InvalidData => "capture erasure position mismatch",
        _ => "capture erasure position unavailable",
    };
    std::io::Error::other(message)
}

pub fn inspect_capture_erasure(expected: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let (journal, witness) = crate::config::capture_erasure_paths();
    if expected.is_some_and(|position| position.len() > 1024) {
        return Err("erasure position is too large".into());
    }
    let expected = expected.map(serde_json::from_str).transpose()?;
    let current = crate::capture_erasure::inspect_position(&journal, &witness, expected.as_ref())?;
    println!("{}", serde_json::to_string(&current)?);
    Ok(())
}

/// Brings the process up: environment, logging, configuration, then serves
/// until a shutdown signal arrives.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut shutdown = Box::pin(shutdown_signal());
    tokio::select! {
        biased;
        _ = &mut shutdown => return Err("startup stopped".into()),
        _ = tokio::task::yield_now() => {},
    }
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
    tokio::select! {
        result = db::ensure_restore_complete(&pool) => result?,
        _ = &mut shutdown => return Err("startup stopped".into()),
    }
    {
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        let migration = db::migrate_notified(&pool, Some(sender));
        tokio::pin!(migration);
        let mut backend = None;
        let mut receiver_open = true;
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => {
                    if backend.is_none() {
                        backend = receiver.try_recv().ok();
                    }
                    if let Some(backend) = backend.as_ref()
                        && !matches!(timeout(Duration::from_secs(1), db::cancel_migration(&config.database_url, backend)).await, Ok(Ok(true)))
                    {
                        tracing::warn!(event.name = "startup_migration_cancel_unconfirmed", "database migration cancellation was not confirmed");
                    }
                    return Err("startup stopped".into());
                }
                identity = &mut receiver, if receiver_open => {
                    backend = identity.ok();
                    receiver_open = false;
                }
                result = &mut migration => {
                    result?;
                    break;
                }
            }
        }
    }
    info!("database schema is up to date");

    let (erasure, replayed) = if config.content_capture_enabled {
        let journal = config.content_capture_erasure_dir.clone();
        let witness = config.content_capture_witness_dir.clone();
        let remote = config
            .content_capture_remote_witness_url
            .as_deref()
            .map(|url| {
                crate::capture_remote_witness::RemoteWitness::new(url, &config.database_url)
                    .map(Arc::new)
            })
            .transpose()?;
        if remote.is_none() {
            tracing::warn!(error.type = "capture_remote_witness_missing", "capture erasure lacks an independent witness");
        }
        let active = tokio::task::spawn_blocking(move || {
            crate::capture_erasure::ActiveErasure::open_with_remote(&journal, &witness, remote)
        })
        .await??;
        let active = Arc::new(active);
        let floor = sqlx::query_file!("queries/capture_erasure_floor_get.sql")
            .fetch_optional(&pool)
            .await?;
        if let Some(floor) = floor {
            let expected = crate::capture_erasure::ErasurePosition::from_database(
                floor.install,
                floor.sequence,
                floor.tip,
            )
            .map_err(erasure_floor_error)?;
            let active = active.clone();
            tokio::task::spawn_blocking(move || active.verify_position(&expected))
                .await?
                .map_err(erasure_floor_error)?;
        } else {
            tracing::warn!(error.type = "capture_erasure_floor_missing", "capture erasure position is not bound");
        }
        let spool = config.content_capture_spool_dir.clone();
        let backup_guard =
            tokio::task::spawn_blocking(move || crate::capture_backup_barrier::try_shared(&spool))
                .await??;
        let replayed = crate::capture_archive::replay_erasure(
            &pool,
            active.clone(),
            &config.content_capture_spool_dir,
            &config.content_capture_key_dir,
            crate::capture_spool::CaptureLimits::default(),
            0,
        )
        .await?;
        drop(backup_guard);
        (Some(active), replayed)
    } else {
        (None, 0)
    };
    let mut state = AppState::new(config, pool)?;
    state.capture_erasure = erasure;
    let state = Arc::new(state);
    let capture_maintenance = if state.config.content_capture_enabled {
        let maintenance = crate::capture_maintenance::CaptureMaintenance::new(
            state.config.content_capture_spool_dir.clone(),
            state.config.content_capture_key_dir.clone(),
            crate::capture_spool::CaptureLimits::default(),
            state
                .capture_erasure
                .clone()
                .ok_or("erasure journal unavailable")?,
            replayed,
        );
        tracing::info!(
            event.name = "capture_startup_cleanup_started",
            "checking capture cleanup"
        );
        tokio::select! {
            result = maintenance.prepare(&state.pool) => result?,
            _ = &mut shutdown => return Err("capture startup cleanup stopped".into()),
        }
        tracing::info!(
            event.name = "capture_startup_cleanup_finished",
            "capture cleanup checked"
        );
        Some(maintenance)
    } else {
        None
    };
    let refresh = tokio::select! {
        biased;
        _ = &mut shutdown => return Err("startup stopped".into()),
        result = model_sync::refresh(&state.pool, &state.client) => result,
    };
    if let Err(error) = refresh {
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
    let capture_maintenance = if let Some(maintenance) = capture_maintenance {
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let pool = state.pool.clone();
        let task = tokio::spawn(async move {
            maintenance
                .run(&pool, Duration::from_secs(30), receiver)
                .await
        });
        Some((stop, task))
    } else {
        None
    };

    info!(%address, "agent pool gateway started");
    let serve_result = serve_with_shutdown(listener, Arc::clone(&state), shutdown).await;
    model_sync.abort();
    if let Some((stop, mut task)) = capture_maintenance {
        let _ = stop.send(true);
        match timeout(state.config.shutdown_finalize_timeout, &mut task).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => {
                tracing::error!(error.type = "capture_maintenance_failed", exception.message = %error)
            }
            Ok(Err(error)) => {
                tracing::error!(error.type = "capture_maintenance_failed", exception.message = %error)
            }
            Err(_) => {
                task.abort();
                tracing::error!(error.type = "capture_maintenance_shutdown_timeout");
            }
        }
    }
    serve_result?;

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
        &config.content_capture_erasure_dir,
        &config.content_capture_witness_dir,
    ])?;
    let pool = db::pool(&config.database_url, 1)?;
    db::ensure_restore_complete(&pool).await?;
    let mut tx = pool.begin().await?;
    let schema = sqlx::query_file!("queries/capture_initialize_schema.sql")
        .fetch_one(&mut *tx)
        .await?;
    if schema.records != schema.usage || schema.records != schema.marks {
        return Err("content storage database schema is incomplete".into());
    }
    if schema.records {
        sqlx::query_file!("queries/capture_initialize_lock.sql")
            .execute(&mut *tx)
            .await?;
        let count: i64 = sqlx::query_file_scalar!("queries/capture_initialize_count.sql")
            .fetch_one(&mut *tx)
            .await?;
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
    let erasure = config.content_capture_erasure_dir;
    let witness = config.content_capture_witness_dir;
    let fingerprint = tokio::task::spawn_blocking(move || {
        let exists = |path: &std::path::Path| match std::fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        };
        let current = exists(&keys.join("current"))?;
        let pending = exists(&keys.join(".initializing"))?;
        let _erasure = if current {
            if !pending {
                return Err(std::io::Error::other(
                    "content storage is already initialized",
                ));
            }
            let active = crate::capture_erasure::ActiveErasure::open(&erasure, &witness)?;
            if !active.verified_records()?.is_empty() {
                return Err(std::io::Error::other("erasure journal is not empty"));
            }
            active
        } else {
            crate::capture_erasure::ActiveErasure::initialize(&erasure, &witness)?
        };
        crate::capture_keyring::initialize(&keys, &anchors, &spool)
            .map_err(|_| std::io::Error::other("content key initialization failed"))
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
    if config.content_capture_enabled {
        crate::capture_paths::validate_paths([
            &config.content_capture_spool_dir,
            &config.content_capture_key_dir,
            &config.content_capture_anchor_dir,
            &config.content_capture_erasure_dir,
            &config.content_capture_witness_dir,
        ])?;
    }
    validate_capture_storage(
        config.content_capture_enabled,
        &config.content_capture_spool_dir,
        &config.content_capture_key_dir,
        &config.content_capture_anchor_dir,
    )?;
    if config.content_capture_enabled
        && config.content_capture_remote_witness_url.is_none()
        && !config.content_capture_test_allow_local_witness
    {
        return Err("independent erasure witness required".into());
    }
    Ok(())
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
    fn erasure_floor_errors_distinguish_busy_mismatch_and_io_failure() {
        use std::io::{Error, ErrorKind};
        for (kind, expected) in [
            (ErrorKind::WouldBlock, "capture erasure journal busy"),
            (ErrorKind::InvalidData, "capture erasure position mismatch"),
            (
                ErrorKind::PermissionDenied,
                "capture erasure position unavailable",
            ),
        ] {
            assert_eq!(
                super::erasure_floor_error(Error::from(kind)).to_string(),
                expected
            );
        }
    }

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
