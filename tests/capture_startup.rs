use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use uuid::Uuid;

#[sqlx::test]
async fn gateway_rejects_incomplete_restore_even_when_capture_is_disabled(pool: sqlx::PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    sqlx::query_file!("queries/test_capture_restore_incomplete.sql")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let base =
        std::path::Path::new("/tmp").join(format!("gateway-restore-check-{}", Uuid::new_v4()));
    std::fs::create_dir(&base).unwrap();
    let mut gateway = Command::new(env!("CARGO_BIN_EXE_agent-pool-gateway"))
        .current_dir(&base)
        .env_clear()
        .env("DATABASE_URL", database.as_str())
        .env("DATABASE_MAX_CONNECTIONS", "5")
        .env("ADMIN_DATABASE_MAX_CONNECTIONS", "1")
        .env(
            "CREDENTIAL_HASH_KEY",
            "fixture-hash-key-with-at-least-32-bytes",
        )
        .env("CREDENTIAL_ENVIRONMENT", "dev")
        .env(
            "PROVIDER_ALLOWED_TENANT_IDS",
            "00000000-0000-0000-0000-000000000001",
        )
        .env("CONTENT_CAPTURE_ENABLED", "false")
        .env("LISTEN_ADDRESS", "127.0.0.1:0")
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
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
    assert!(exited, "gateway did not reject an incomplete restore");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("capture restore incomplete"));
    let pool = sqlx::PgPool::connect(database.as_str()).await.unwrap();
    sqlx::query_file!("queries/test_capture_restore_complete.sql")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut gateway = Command::new(env!("CARGO_BIN_EXE_agent-pool-gateway"))
        .current_dir(&base)
        .env_clear()
        .env("DATABASE_URL", database.as_str())
        .env("DATABASE_MAX_CONNECTIONS", "5")
        .env("ADMIN_DATABASE_MAX_CONNECTIONS", "1")
        .env(
            "CREDENTIAL_HASH_KEY",
            "fixture-hash-key-with-at-least-32-bytes",
        )
        .env("CREDENTIAL_ENVIRONMENT", "dev")
        .env(
            "PROVIDER_ALLOWED_TENANT_IDS",
            "00000000-0000-0000-0000-000000000001",
        )
        .env("CONTENT_CAPTURE_ENABLED", "false")
        .env("LISTEN_ADDRESS", address.to_string())
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let client = reqwest::Client::new();
    let reachable = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(gateway.try_wait().unwrap().is_none());
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
    .await;
    gateway.kill().unwrap();
    gateway.wait().unwrap();
    std::fs::remove_dir_all(base).unwrap();
    assert!(reachable.is_ok());
}

#[test]
fn gateway_rejects_missing_capture_storage_before_database_connection() {
    let base = std::path::Path::new("/tmp").join(format!("gateway-start-check-{}", Uuid::new_v4()));
    std::fs::create_dir(&base).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let spool = base.join("spool");
    let keys = base.join("keys");
    let anchors = base.join("anchors");
    let erasure = base.join("erasure");
    let witness = base.join("witness");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_agent-pool-gateway"))
            .current_dir(&base)
            .env_clear()
            .env("DATABASE_URL", "postgres://gateway@127.0.0.1:1/gateway")
            .env("DATABASE_MAX_CONNECTIONS", "5")
            .env("ADMIN_DATABASE_MAX_CONNECTIONS", "1")
            .env(
                "CREDENTIAL_HASH_KEY",
                "fixture-hash-key-with-at-least-32-bytes",
            )
            .env("CREDENTIAL_ENVIRONMENT", "dev")
            .env(
                "PROVIDER_ALLOWED_TENANT_IDS",
                "00000000-0000-0000-0000-000000000001",
            )
            .env("CONTENT_CAPTURE_ENABLED", "true")
            .env("CONTENT_CAPTURE_SPOOL_DIR", &spool)
            .env("CONTENT_CAPTURE_KEY_DIR", &keys)
            .env("CONTENT_CAPTURE_ANCHOR_DIR", &anchors)
            .env("CONTENT_CAPTURE_ERASURE_DIR", &erasure)
            .env("CONTENT_CAPTURE_WITNESS_DIR", &witness)
            .output()
            .unwrap()
    };
    let missing = run();
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr)
            .contains("content storage directories are unavailable or unsafe"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    for path in [&spool, &keys, &anchors, &erasure, &witness] {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let no_key = run();
    assert!(!no_key.status.success());
    assert!(String::from_utf8_lossy(&no_key.stderr).contains("capture keyring unavailable"));
}
