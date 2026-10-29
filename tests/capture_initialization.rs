use sqlx::PgPool;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use uuid::Uuid;

fn make_dirs(base: &Path) {
    std::fs::create_dir(base).unwrap();
    std::fs::set_permissions(base, std::fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["spool", "keys", "anchors"] {
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
        .env("DATABASE_MAX_CONNECTIONS", "3")
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
        .env("CONTENT_CAPTURE_SPOOL_DIR", base.join("spool"))
        .env("CONTENT_CAPTURE_KEY_DIR", base.join("keys"))
        .env("CONTENT_CAPTURE_ANCHOR_DIR", base.join("anchors"));
    command
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
    let client = reqwest::Client::new();
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
    drop(gateway);
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
