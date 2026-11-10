use sqlx::PgPool;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use uuid::Uuid;

fn make_dirs(base: &Path) {
    std::fs::create_dir(base).unwrap();
    std::fs::set_permissions(base, std::fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["spool", "keys", "anchors", "erasure", "witness"] {
        let path = base.join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn command(base: &Path, database: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-pool-gateway"));
    command
        .current_dir(base)
        .env_clear()
        .env("DATABASE_URL", database)
        .env("DATABASE_MAX_CONNECTIONS", "5")
        .env("ADMIN_DATABASE_MAX_CONNECTIONS", "1")
        .env(
            "CREDENTIAL_HASH_KEY",
            "fixture-hash-key-with-at-least-32-bytes",
        )
        .env("CREDENTIAL_ENVIRONMENT", "dev")
        .env("CONTENT_CAPTURE_TEST_ALLOW_LOCAL_WITNESS", "1")
        .env(
            "PROVIDER_ALLOWED_TENANT_IDS",
            "00000000-0000-0000-0000-000000000001",
        )
        .env("CONTENT_CAPTURE_SPOOL_DIR", base.join("spool"))
        .env("CONTENT_CAPTURE_KEY_DIR", base.join("keys"))
        .env("CONTENT_CAPTURE_ANCHOR_DIR", base.join("anchors"))
        .env("CONTENT_CAPTURE_ERASURE_DIR", base.join("erasure"))
        .env("CONTENT_CAPTURE_WITNESS_DIR", base.join("witness"));
    command
}

#[sqlx::test]
async fn startup_rejects_missing_remote_witness(pool: PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    pool.close().await;
    let base =
        std::path::Path::new("/tmp").join(format!("capture-remote-required-{}", Uuid::new_v4()));
    make_dirs(&base);
    let initialized = initialize(&base, database.as_str());
    assert!(initialized.status.success());
    let events = std::fs::read(base.join("erasure/events")).unwrap();
    let head = std::fs::read(base.join("witness/head")).unwrap();
    let mut gateway = command(&base, database.as_str());
    gateway
        .env("CONTENT_CAPTURE_ENABLED", "true")
        .env_remove("CONTENT_CAPTURE_REMOTE_WITNESS_URL");
    if cfg!(debug_assertions) {
        gateway.env_remove("CONTENT_CAPTURE_TEST_ALLOW_LOCAL_WITNESS");
    } else {
        gateway.env("CONTENT_CAPTURE_TEST_ALLOW_LOCAL_WITNESS", "1");
    }
    let output = gateway.output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("independent erasure witness required")
    );
    assert_eq!(std::fs::read(base.join("erasure/events")).unwrap(), events);
    assert_eq!(std::fs::read(base.join("witness/head")).unwrap(), head);
    std::fs::remove_dir_all(base).unwrap();
}

#[sqlx::test]
async fn startup_stops_while_database_migration_is_locked(pool: PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    let base =
        std::path::Path::new("/tmp").join(format!("gateway-migration-stop-{}", Uuid::new_v4()));
    make_dirs(&base);
    let mut locked = pool.begin().await.unwrap();
    sqlx::query_file!("queries/test_lock_migrations.sql")
        .execute(&mut *locked)
        .await
        .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let start = || {
        command(&base, database.as_str())
            .env("CONTENT_CAPTURE_ENABLED", "false")
            .env("LISTEN_ADDRESS", address.to_string())
            .env("LEDGER_SPOOL_DIR", base.join("ledger"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut gateway = TestGateway(start());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            assert!(gateway.0.try_wait().unwrap().is_none());
            if sqlx::query_file_scalar!("queries/test_waiting_migration.sql")
                .fetch_one(&pool)
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        std::process::Command::new("kill")
            .arg("-TERM")
            .arg(gateway.0.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if gateway.0.try_wait().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok();
    drop(gateway);
    locked.rollback().await.unwrap();
    assert!(
        sqlx::query_file_scalar!("queries/test_migrations_clean.sql")
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    let mut restarted = TestGateway(start());
    let client = reqwest::Client::new();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(restarted.0.try_wait().unwrap().is_none());
            if client
                .get(format!("http://{address}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    drop(restarted);
    std::fs::remove_dir_all(base).unwrap();
    assert!(stopped, "gateway did not stop while migration was blocked");
}

#[tokio::test]
async fn startup_stops_while_database_connection_is_unresponsive() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let database = format!(
        "postgres://fixture@{}/gateway",
        listener.local_addr().unwrap()
    );
    let base =
        std::path::Path::new("/tmp").join(format!("gateway-connect-stop-{}", Uuid::new_v4()));
    make_dirs(&base);
    let child = command(&base, &database)
        .env("CONTENT_CAPTURE_ENABLED", "false")
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut gateway = TestGateway(child);
    let (connection, _) =
        tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
    assert!(
        std::process::Command::new("kill")
            .arg("-TERM")
            .arg(gateway.0.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if gateway.0.try_wait().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok();
    drop(gateway);
    drop(connection);
    std::fs::remove_dir_all(base).unwrap();
    assert!(
        stopped,
        "gateway did not stop while database connection was unresponsive"
    );
}

#[sqlx::test]
async fn startup_stops_while_model_discovery_is_unresponsive(pool: PgPool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = format!("http://{}", listener.local_addr().unwrap());
    sqlx::query_file!("queries/test_insert_hanging_ollama.sql", provider_url)
        .execute(&pool)
        .await
        .unwrap();
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    let base = std::path::Path::new("/tmp").join(format!("gateway-model-stop-{}", Uuid::new_v4()));
    make_dirs(&base);
    let child = command(&base, database.as_str())
        .env("CONTENT_CAPTURE_ENABLED", "false")
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut gateway = TestGateway(child);
    let (connection, _) =
        tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
    assert!(
        std::process::Command::new("kill")
            .arg("-TERM")
            .arg(gateway.0.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(status) = gateway.0.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    drop(gateway);
    drop(connection);
    std::fs::remove_dir_all(base).unwrap();
    let status = stopped.expect("gateway did not stop while model discovery was unresponsive");
    assert_eq!(status.signal(), None);
}

#[sqlx::test]
async fn startup_stop_rolls_back_model_sync_and_restart_recovers(pool: PgPool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = format!("http://{}", listener.local_addr().unwrap());
    let router = axum::Router::new()
        .route(
            "/api/tags",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({"models": [{"name": "first"}, {"name": "second"}]}))
            }),
        )
        .route(
            "/api/show",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({"capabilities": ["completion"]}))
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    sqlx::query_file!("queries/test_insert_hanging_ollama.sql", provider_url)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query_file!("queries/test_insert_model_for_stop.sql")
        .execute(&pool)
        .await
        .unwrap();
    let mut lock = pool.begin().await.unwrap();
    sqlx::query_file!("queries/test_lock_model_for_stop.sql")
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    let base =
        std::path::Path::new("/tmp").join(format!("gateway-model-write-stop-{}", Uuid::new_v4()));
    make_dirs(&base);
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let start = || {
        command(&base, database.as_str())
            .env("CONTENT_CAPTURE_ENABLED", "false")
            .env("LISTEN_ADDRESS", address.to_string())
            .env("LEDGER_SPOOL_DIR", base.join("ledger"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut gateway = TestGateway(start());
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            assert!(gateway.0.try_wait().unwrap().is_none());
            if sqlx::query_file_scalar!("queries/test_waiting_model_write.sql")
                .fetch_one(&pool)
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        Command::new("kill")
            .arg("-TERM")
            .arg(gateway.0.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    let status = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(status) = gateway.0.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    drop(gateway);
    lock.rollback().await.unwrap();
    let status = status.expect("gateway did not stop during model sync");
    assert_eq!(status.signal(), None);
    assert_eq!(status.code(), Some(1));
    let state = sqlx::query_file!("queries/test_model_stop_state.sql")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((state.first, state.original), (0, 1));
    let mut restarted = TestGateway(start());
    let client = reqwest::Client::new();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(restarted.0.try_wait().unwrap().is_none());
            if client
                .get(format!("http://{address}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let state = sqlx::query_file!("queries/test_model_stop_state.sql")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((state.first, state.active), (1, 2));
    assert_ne!((state.first, state.original), (0, 1));
    drop(restarted);
    server.abort();
    std::fs::remove_dir_all(base).unwrap();
}

#[sqlx::test]
async fn startup_stop_rolls_back_partially_applied_migration(pool: PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    sqlx::query_file!("queries/test_drop_erasure_floor.sql")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query_file!("queries/test_remove_floor_migration.sql")
        .execute(&pool)
        .await
        .unwrap();
    let mut locked = pool.begin().await.unwrap();
    sqlx::query_file!("queries/test_lock_migration_writes.sql")
        .execute(&mut *locked)
        .await
        .unwrap();
    let base =
        std::path::Path::new("/tmp").join(format!("gateway-migration-rollback-{}", Uuid::new_v4()));
    make_dirs(&base);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let start = || {
        command(&base, database.as_str())
            .env("CONTENT_CAPTURE_ENABLED", "false")
            .env("LISTEN_ADDRESS", address.to_string())
            .env("LEDGER_SPOOL_DIR", base.join("ledger"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut gateway = TestGateway(start());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            assert!(gateway.0.try_wait().unwrap().is_none());
            if sqlx::query_file_scalar!("queries/test_waiting_migration_write.sql")
                .fetch_one(&pool)
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        std::process::Command::new("kill")
            .arg("-TERM")
            .arg(gateway.0.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if gateway.0.try_wait().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    drop(gateway);
    locked.rollback().await.unwrap();
    assert!(
        !sqlx::query_file_scalar!("queries/test_floor_exists.sql")
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    assert!(
        !sqlx::query_file_scalar!("queries/test_floor_migration_applied.sql")
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    let mut restarted = TestGateway(start());
    let client = reqwest::Client::new();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(restarted.0.try_wait().unwrap().is_none());
            if client
                .get(format!("http://{address}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        sqlx::query_file_scalar!("queries/test_floor_exists.sql")
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    assert!(
        sqlx::query_file_scalar!("queries/test_floor_migration_applied.sql")
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    drop(restarted);
    std::fs::remove_dir_all(base).unwrap();
}

#[sqlx::test]
async fn startup_stop_cancels_server_side_migration(pool: PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    sqlx::query_file!("queries/test_drop_erasure_floor.sql")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query_file!("queries/test_remove_floor_migration.sql")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query_file!("queries/test_delay_floor_function.sql")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query_file!("queries/test_delay_floor_trigger.sql")
        .execute(&pool)
        .await
        .unwrap();
    let other_pool = pool.clone();
    let other = tokio::spawn(async move {
        sqlx::query_file_scalar!("queries/test_unrelated_pause.sql")
            .fetch_one(&other_pool)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if sqlx::query_file_scalar!("queries/test_waiting_unrelated.sql")
                .fetch_one(&pool)
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let base =
        std::path::Path::new("/tmp").join(format!("gateway-migration-cancel-{}", Uuid::new_v4()));
    make_dirs(&base);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let start = || {
        command(&base, database.as_str())
            .env("CONTENT_CAPTURE_ENABLED", "false")
            .env("LISTEN_ADDRESS", address.to_string())
            .env("LEDGER_SPOOL_DIR", base.join("ledger"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut gateway = TestGateway(start());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            assert!(gateway.0.try_wait().unwrap().is_none());
            if sqlx::query_file_scalar!("queries/test_waiting_floor_delay.sql")
                .fetch_one(&pool)
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        std::process::Command::new("kill")
            .arg("-TERM")
            .arg(gateway.0.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if gateway.0.try_wait().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    drop(gateway);
    let released = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !sqlx::query_file_scalar!("queries/test_waiting_floor_delay.sql")
                .fetch_one(&pool)
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok();
    if released {
        assert!(
            !other.is_finished(),
            "unrelated query finished before the cancellation check"
        );
        assert!(other.await.unwrap().is_ok());
        assert!(
            !sqlx::query_file_scalar!("queries/test_floor_exists.sql")
                .fetch_one(&pool)
                .await
                .unwrap()
        );
        assert!(
            !sqlx::query_file_scalar!("queries/test_floor_migration_applied.sql")
                .fetch_one(&pool)
                .await
                .unwrap()
        );
        sqlx::query_file!("queries/test_drop_delay_floor_trigger.sql")
            .execute(&pool)
            .await
            .unwrap();
        let mut restarted = TestGateway(start());
        let client = reqwest::Client::new();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                assert!(restarted.0.try_wait().unwrap().is_none());
                if client
                    .get(format!("http://{address}/health"))
                    .send()
                    .await
                    .is_ok_and(|response| response.status().is_success())
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            sqlx::query_file_scalar!("queries/test_floor_migration_applied.sql")
                .fetch_one(&pool)
                .await
                .unwrap()
        );
        drop(restarted);
    }
    std::fs::remove_dir_all(base).unwrap();
    assert!(
        released,
        "database continued the abandoned migration after gateway stopped"
    );
}

#[sqlx::test]
async fn startup_refuses_to_serve_while_expired_capture_is_locked(pool: PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    pool.close().await;
    let base =
        std::path::Path::new("/tmp").join(format!("capture-expired-startup-{}", Uuid::new_v4()));
    make_dirs(&base);
    assert!(initialize(&base, database.as_str()).status.success());
    let pool = PgPool::connect(database.as_str()).await.unwrap();
    let tenant = Uuid::new_v4();
    let capture = Uuid::new_v4();
    sqlx::query_file!(
        "queries/test_capture_expired_insert.sql",
        Uuid::new_v4(),
        tenant,
        capture
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut locked = pool.begin().await.unwrap();
    assert!(
        sqlx::query_file_scalar!("queries/capture_expire_recheck.sql", tenant, capture)
            .fetch_optional(&mut *locked)
            .await
            .unwrap()
            .is_some()
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let child = command(&base, database.as_str())
        .env("CONTENT_CAPTURE_ENABLED", "true")
        .env("LISTEN_ADDRESS", address.to_string())
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut gateway = TestGateway(child);
    let client = reqwest::Client::new();
    let healthy = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if client
                .get(format!("http://{address}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return true;
            }
            if gateway.0.try_wait().unwrap().is_some() {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or(false);
    locked.rollback().await.unwrap();
    if !healthy {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                assert!(gateway.0.try_wait().unwrap().is_none());
                if client
                    .get(format!("http://{address}/health"))
                    .send()
                    .await
                    .is_ok_and(|response| response.status().is_success())
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            sqlx::query_file_scalar!("queries/test_capture_deleted.sql", tenant, capture)
                .fetch_one(&pool)
                .await
                .unwrap()
        );
    }
    drop(gateway);
    std::fs::remove_dir_all(base).unwrap();
    assert!(
        !healthy,
        "gateway served while expired capture could not be erased"
    );
}

#[sqlx::test]
async fn startup_cleans_more_than_one_expiration_batch(pool: PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    pool.close().await;
    let base =
        std::path::Path::new("/tmp").join(format!("capture-expired-batches-{}", Uuid::new_v4()));
    make_dirs(&base);
    assert!(initialize(&base, database.as_str()).status.success());
    let pool = PgPool::connect(database.as_str()).await.unwrap();
    let tenant = Uuid::new_v4();
    for _ in 0..33 {
        sqlx::query_file!(
            "queries/test_capture_expired_insert.sql",
            Uuid::new_v4(),
            tenant,
            Uuid::new_v4()
        )
        .execute(&pool)
        .await
        .unwrap();
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let child = command(&base, database.as_str())
        .env("CONTENT_CAPTURE_ENABLED", "true")
        .env("LISTEN_ADDRESS", address.to_string())
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut gateway = TestGateway(child);
    let client = reqwest::Client::new();
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            assert!(gateway.0.try_wait().unwrap().is_none());
            if client
                .get(format!("http://{address}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_file_scalar!("queries/test_capture_deleted_count.sql", tenant)
            .fetch_one(&pool)
            .await
            .unwrap(),
        33
    );
    drop(gateway);
    std::fs::remove_dir_all(base).unwrap();
}

#[sqlx::test]
async fn startup_cleanup_resumes_after_process_interruption(pool: PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    pool.close().await;
    let base =
        std::path::Path::new("/tmp").join(format!("capture-expired-resume-{}", Uuid::new_v4()));
    make_dirs(&base);
    assert!(initialize(&base, database.as_str()).status.success());
    let pool = PgPool::connect(database.as_str()).await.unwrap();
    let tenant = Uuid::new_v4();
    let blocked = Uuid::new_v4();
    let ready = Uuid::new_v4();
    for capture in [blocked, ready] {
        sqlx::query_file!(
            "queries/test_capture_expired_insert.sql",
            Uuid::new_v4(),
            tenant,
            capture
        )
        .execute(&pool)
        .await
        .unwrap();
    }
    let mut locked = pool.begin().await.unwrap();
    assert!(
        sqlx::query_file_scalar!("queries/capture_expire_recheck.sql", tenant, blocked)
            .fetch_optional(&mut *locked)
            .await
            .unwrap()
            .is_some()
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let start = || {
        command(&base, database.as_str())
            .env("CONTENT_CAPTURE_ENABLED", "true")
            .env("LISTEN_ADDRESS", address.to_string())
            .env("LEDGER_SPOOL_DIR", base.join("ledger"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut first = TestGateway(start());
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(first.0.try_wait().unwrap().is_none());
            if sqlx::query_file_scalar!("queries/test_capture_deleted.sql", tenant, ready)
                .fetch_one(&pool)
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let signalled = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(first.0.id().to_string())
        .status()
        .unwrap();
    assert!(signalled.success());
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(status) = first.0.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(!status.success());
    drop(first);
    let mut interrupted = TestGateway(start());
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(interrupted.0.try_wait().unwrap().is_none());
    interrupted.0.kill().unwrap();
    drop(interrupted);
    locked.rollback().await.unwrap();
    let mut second = TestGateway(start());
    let client = reqwest::Client::new();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(second.0.try_wait().unwrap().is_none());
            if client
                .get(format!("http://{address}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    for capture in [blocked, ready] {
        assert!(
            sqlx::query_file_scalar!("queries/test_capture_deleted.sql", tenant, capture)
                .fetch_one(&pool)
                .await
                .unwrap()
        );
    }
    drop(second);
    std::fs::remove_dir_all(base).unwrap();
}

#[sqlx::test]
async fn startup_retains_short_spool_but_rejects_full_length_corruption(pool: PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    pool.close().await;
    let base =
        std::path::Path::new("/tmp").join(format!("capture-corrupt-startup-{}", Uuid::new_v4()));
    make_dirs(&base);
    assert!(initialize(&base, database.as_str()).status.success());
    let short = base
        .join("spool")
        .join(format!("{}.capture", Uuid::new_v4()));
    std::fs::write(&short, vec![0u8; 157]).unwrap();
    std::fs::set_permissions(&short, std::fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let start = || {
        command(&base, database.as_str())
            .env("CONTENT_CAPTURE_ENABLED", "true")
            .env("LISTEN_ADDRESS", address.to_string())
            .env("LEDGER_SPOOL_DIR", base.join("ledger"))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let mut first = TestGateway(start());
    let client = reqwest::Client::new();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(first.0.try_wait().unwrap().is_none());
            if client
                .get(format!("http://{address}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(&short).unwrap(), vec![0u8; 157]);
    drop(first);
    let full = base
        .join("spool")
        .join(format!("{}.capture", Uuid::new_v4()));
    std::fs::write(&full, vec![0u8; 158]).unwrap();
    std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut second = start();
    let exited = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if second.try_wait().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok();
    if !exited {
        second.kill().unwrap();
    }
    let output = second.wait_with_output().unwrap();
    assert!(exited && !output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("capture archive unavailable"));
    assert_eq!(std::fs::read(&short).unwrap(), vec![0u8; 157]);
    assert_eq!(std::fs::read(&full).unwrap(), vec![0u8; 158]);
    std::fs::remove_dir_all(base).unwrap();
}

fn initialize(base: &Path, database: &str) -> std::process::Output {
    command(base, database)
        .arg("--initialize-content-capture")
        .output()
        .unwrap()
}

struct TestGateway(Child);

impl Drop for TestGateway {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[sqlx::test]
async fn initialization_does_not_touch_storage_during_incomplete_restore(pool: PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    sqlx::query_file!("queries/test_capture_restore_incomplete.sql")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let base =
        std::path::Path::new("/tmp").join(format!("capture-restore-init-{}", Uuid::new_v4()));
    make_dirs(&base);
    let result = initialize(&base, database.as_str());
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("capture restore incomplete"));
    assert!(!base.join("keys/current").exists());
    assert!(!base.join("erasure/events").exists());
    std::fs::remove_dir_all(base).unwrap();
}

#[sqlx::test]
async fn startup_rejects_a_journal_outside_the_database_floor(pool: PgPool) {
    use sha2::{Digest, Sha256};
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    let database = database.to_string();
    pool.close().await;
    let base = std::path::Path::new("/tmp").join(format!("capture-floor-{}", Uuid::new_v4()));
    make_dirs(&base);
    let initialized = initialize(&base, &database);
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let output = command(&base, &database)
        .arg("--inspect-capture-erasure")
        .output()
        .unwrap();
    assert!(output.status.success());
    let position: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let install = Uuid::parse_str(position["install"].as_str().unwrap()).unwrap();
    let sequence = position["sequence"].as_i64().unwrap();
    let tip: Vec<u8> = serde_json::from_value(position["tip"].clone()).unwrap();
    let setup = PgPool::connect(&database).await.unwrap();
    sqlx::query_file!(
        "queries/test_capture_erasure_floor_insert.sql",
        install,
        sequence,
        &tip
    )
    .execute(&setup)
    .await
    .unwrap();
    setup.close().await;
    let journal = base.join("other-erasure");
    let witness = base.join("other-witness");
    for dir in [&journal, &witness] {
        std::fs::create_dir(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let other = Uuid::new_v4();
    let mut header = b"APGEJ001".to_vec();
    header.extend_from_slice(other.as_bytes());
    let mut head = b"APGEW001".to_vec();
    head.extend_from_slice(other.as_bytes());
    head.extend_from_slice(&0u64.to_be_bytes());
    head.extend_from_slice(&Sha256::digest(&header));
    head.extend_from_slice(&Sha256::digest(&head));
    for (path, bytes) in [
        (journal.join("events"), header),
        (witness.join("head"), head),
    ] {
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut gateway = command(&base, &database)
        .env("CONTENT_CAPTURE_ENABLED", "true")
        .env("CONTENT_CAPTURE_ERASURE_DIR", journal)
        .env("CONTENT_CAPTURE_WITNESS_DIR", witness)
        .env("LISTEN_ADDRESS", "127.0.0.1:0")
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut exited = false;
    for _ in 0..100 {
        if gateway.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if !exited {
        gateway.kill().unwrap();
    }
    let output = gateway.wait_with_output().unwrap();
    assert!(exited, "gateway did not reject another journal");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("capture erasure position mismatch"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let child = command(&base, &database)
        .env("CONTENT_CAPTURE_ENABLED", "true")
        .env("LISTEN_ADDRESS", address.to_string())
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut valid = TestGateway(child);
    let client = reqwest::Client::new();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(valid.0.try_wait().unwrap().is_none());
            if client
                .get(format!("http://{address}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    drop(valid);
    std::fs::remove_dir_all(base).unwrap();
}

#[sqlx::test]
async fn journal_position_command_rejects_a_different_installation(pool: PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    let database = database.to_string();
    pool.close().await;
    let base = std::path::Path::new("/tmp").join(format!("capture-position-{}", Uuid::new_v4()));
    make_dirs(&base);
    let initialized = initialize(&base, &database);
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let before = std::fs::read(base.join("erasure/events")).unwrap();
    std::fs::write(
        base.join(".env"),
        format!(
            "CONTENT_CAPTURE_ERASURE_DIR={}\nCONTENT_CAPTURE_WITNESS_DIR={}\n",
            base.join("erasure").display(),
            base.join("witness").display()
        ),
    )
    .unwrap();
    let position = command(&base, &database)
        .env_remove("DATABASE_URL")
        .env_remove("CREDENTIAL_HASH_KEY")
        .env_remove("CONTENT_CAPTURE_ERASURE_DIR")
        .env_remove("CONTENT_CAPTURE_WITNESS_DIR")
        .arg("--inspect-capture-erasure")
        .output()
        .unwrap();
    assert!(
        position.status.success(),
        "{}",
        String::from_utf8_lossy(&position.stderr)
    );
    let original = String::from_utf8(position.stdout).unwrap();
    let accepted = command(&base, &database)
        .arg("--inspect-capture-erasure")
        .arg(original.trim())
        .output()
        .unwrap();
    assert!(accepted.status.success());
    let mut other: serde_json::Value = serde_json::from_str(&original).unwrap();
    other["install"] = Uuid::new_v4().to_string().into();
    let rejected = command(&base, &database)
        .arg("--inspect-capture-erasure")
        .arg(other.to_string())
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert_eq!(std::fs::read(base.join("erasure/events")).unwrap(), before);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let child = command(&base, &database)
        .env("CONTENT_CAPTURE_ENABLED", "true")
        .env("LISTEN_ADDRESS", address.to_string())
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut gateway = TestGateway(child);
    let client = reqwest::Client::new();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(gateway.0.try_wait().unwrap().is_none());
            if client
                .get(format!("http://{address}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let live = command(&base, &database)
        .arg("--inspect-capture-erasure")
        .arg(original.trim())
        .output()
        .unwrap();
    assert!(
        live.status.success(),
        "{}",
        String::from_utf8_lossy(&live.stderr)
    );
    assert!(gateway.0.try_wait().unwrap().is_none());
    drop(gateway);
    std::fs::remove_dir_all(base).unwrap();
}

#[sqlx::test]
async fn startup_replay_rejects_active_capture_backup(pool: PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    let database = database.to_string();
    pool.close().await;
    let base = std::path::Path::new("/tmp").join(format!("capture-init-backup-{}", Uuid::new_v4()));
    make_dirs(&base);
    let initialized = initialize(&base, &database);
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(base.join("spool/.capture-backup.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let mut startup = command(&base, &database)
        .env("CONTENT_CAPTURE_ENABLED", "true")
        .env("LISTEN_ADDRESS", "127.0.0.1:0")
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut exited = false;
    for _ in 0..100 {
        if startup.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if !exited {
        startup.kill().unwrap();
    }
    let output = startup.wait_with_output().unwrap();
    drop(lock);
    assert!(exited, "gateway did not reject an active backup");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("capture backup coordination busy"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let child = command(&base, &database)
        .env("CONTENT_CAPTURE_ENABLED", "true")
        .env("LISTEN_ADDRESS", address.to_string())
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut gateway = TestGateway(child);
    let client = reqwest::Client::new();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(gateway.0.try_wait().unwrap().is_none());
            if client
                .get(format!("http://{address}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    drop(gateway);
    std::fs::remove_dir_all(base).unwrap();
}

#[sqlx::test]
async fn initialization_resumes_a_persisted_key_without_replacing_it(pool: PgPool) {
    use sha2::{Digest, Sha256};
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    let database = database.to_string();
    pool.close().await;
    let base = std::path::Path::new("/tmp").join(format!("capture-init-resume-{}", Uuid::new_v4()));
    make_dirs(&base);
    let key = [7u8; 32];
    let mut intent = b"APGKINI1".to_vec();
    intent.extend_from_slice(&key);
    let checksum = Sha256::digest(&intent);
    intent.extend_from_slice(&checksum);
    let marker = base.join("keys/.initializing");
    std::fs::write(&marker, &intent).unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    let output = initialize(&base, &database);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(base.join("keys/current")).unwrap(),
        Sha256::digest(key).as_slice()
    );
    assert!(!marker.exists());
    std::fs::write(&marker, intent).unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    let resumed = initialize(&base, &database);
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert_eq!(
        std::fs::read(base.join("keys/current")).unwrap(),
        Sha256::digest(key).as_slice()
    );
    assert!(!marker.exists());
    let mut intent = b"APGKINI1".to_vec();
    intent.extend_from_slice(&key);
    intent.extend_from_slice(&Sha256::digest(&intent));
    std::fs::write(&marker, intent).unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    let current = base.join("keys/current");
    std::fs::rename(&current, base.join("held-current")).unwrap();
    let resumed_without_current = initialize(&base, &database);
    assert!(
        resumed_without_current.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed_without_current.stderr)
    );
    assert_eq!(
        std::fs::read(&current).unwrap(),
        Sha256::digest(key).as_slice()
    );
    let mut intent = b"APGKINI1".to_vec();
    intent.extend_from_slice(&key);
    intent.extend_from_slice(&Sha256::digest(&intent));
    std::fs::write(&marker, intent).unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut startup = command(&base, &database)
        .env("CONTENT_CAPTURE_ENABLED", "true")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut exited = false;
    for _ in 0..100 {
        if startup.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if !exited {
        startup.kill().unwrap();
    }
    let output = startup.wait_with_output().unwrap();
    assert!(exited);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("capture keyring unavailable"));
    assert!(marker.exists());
    std::fs::remove_dir_all(base).unwrap();
}

#[sqlx::test]
async fn initialization_requires_empty_database_and_never_replaces_existing_key(pool: PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    let database = database.to_string();
    pool.close().await;
    let base = std::path::Path::new("/tmp").join(format!("capture-init-test-{}", Uuid::new_v4()));
    make_dirs(&base);
    let first = initialize(&base, &database);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let current = std::fs::read(base.join("keys/current")).unwrap();
    assert_eq!(current.len(), 32);
    let second = initialize(&base, &database);
    assert!(!second.status.success());
    assert_eq!(std::fs::read(base.join("keys/current")).unwrap(), current);
    let data_pool = PgPool::connect(&database).await.unwrap();
    let other =
        std::path::Path::new("/tmp").join(format!("capture-init-reject-{}", Uuid::new_v4()));
    make_dirs(&other);
    let busy = initialize(&other, &database);
    assert!(!busy.status.success());
    assert!(
        String::from_utf8_lossy(&busy.stderr)
            .contains("content storage initialization requires an isolated database")
    );
    assert!(!other.join("keys/current").exists());
    let user = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let capture = Uuid::new_v4();
    sqlx::query_file!(
        "queries/test_capture_expired_insert.sql",
        user,
        tenant,
        capture
    )
    .execute(&data_pool)
    .await
    .unwrap();
    assert!(
        !sqlx::query_file_scalar!("queries/test_capture_deleted.sql", tenant, capture)
            .fetch_one(&data_pool)
            .await
            .unwrap()
    );
    let disabled_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let disabled_address = disabled_listener.local_addr().unwrap();
    drop(disabled_listener);
    let disabled_child = command(&base, &database)
        .env("LISTEN_ADDRESS", disabled_address.to_string())
        .env("CONTENT_CAPTURE_ENABLED", "false")
        .env("LEDGER_SPOOL_DIR", base.join("ledger-disabled"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut disabled_gateway = TestGateway(disabled_child);
    let client = reqwest::Client::new();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(disabled_gateway.0.try_wait().unwrap().is_none());
            if client
                .get(format!("http://{disabled_address}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !sqlx::query_file_scalar!("queries/test_capture_deleted.sql", tenant, capture)
            .fetch_one(&data_pool)
            .await
            .unwrap()
    );
    drop(disabled_gateway);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let child = command(&base, &database)
        .env("LISTEN_ADDRESS", address.to_string())
        .env("ENABLE_DEVELOPMENT_BOOTSTRAP", "true")
        .env("BOOTSTRAP_CLIENT_KEY", "fixture-client-key")
        .env("OPENAI_API_KEY", "fixture-provider-key")
        .env("OPENAI_BASE_URL", "http://127.0.0.1:1")
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut gateway = TestGateway(child);
    let health = format!("http://{address}/health");
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(gateway.0.try_wait().unwrap().is_none());
            if client
                .get(&health)
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let response = client.post(format!("http://{address}/v1/chat/completions"))
        .bearer_auth("fixture-client-key")
        .json(&serde_json::json!({"model":"company-default","messages":[{"role":"user","content":"hello"}]}))
        .send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "capture_unavailable");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if sqlx::query_file_scalar!("queries/test_capture_deleted.sql", tenant, capture)
                .fetch_one(&data_pool)
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    drop(gateway);
    sqlx::query_file!("queries/test_capture_fixture_remove.sql", tenant, capture)
        .execute(&data_pool)
        .await
        .unwrap();
    sqlx::query_file!("queries/capture_erasure_mark.sql", tenant, capture)
        .execute(&data_pool)
        .await
        .unwrap();
    data_pool.close().await;
    let marker_rejected = initialize(&other, &database);
    assert!(!marker_rejected.status.success());
    assert!(
        String::from_utf8_lossy(&marker_rejected.stderr)
            .contains("content storage database already has records")
    );
    assert!(!other.join("keys/current").exists());
    let data_pool = PgPool::connect(&database).await.unwrap();
    sqlx::query_file!(
        "queries/test_capture_erasure_fixture_remove.sql",
        tenant,
        capture
    )
    .execute(&data_pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO capture_master_key_usage (key_fingerprint, wrap_count) VALUES (decode(repeat('01',32),'hex'), 1)")
        .execute(&data_pool).await.unwrap();
    data_pool.close().await;
    let rejected = initialize(&other, &database);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("content storage database already has records")
    );
    assert!(!other.join("keys/current").exists());
}
