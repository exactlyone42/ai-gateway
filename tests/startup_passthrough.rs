use std::process::{Command, Stdio};
use std::time::Duration;
use uuid::Uuid;

#[sqlx::test]
async fn passthrough_stays_available_after_guard_check_interval(pool: sqlx::PgPool) {
    let database_name = pool.connect_options().get_database().unwrap().to_owned();
    let mut database = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    database.set_path(&database_name);
    pool.close().await;
    let base = std::env::temp_dir().join(format!("gateway-passthrough-{}", Uuid::new_v4()));
    std::fs::create_dir(&base).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let log_path = base.join("gateway.log");
    let log = std::fs::File::create(&log_path).unwrap();
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
        .env("SCHEDULER_PASSTHROUGH", "true")
        .env("LISTEN_ADDRESS", address.to_string())
        .env("LEDGER_SPOOL_DIR", base.join("ledger"))
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let healthy = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if gateway.try_wait().unwrap().is_some() {
                return false;
            }
            if std::fs::read_to_string(&log_path)
                .unwrap()
                .contains("agent pool gateway started")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !client
            .get(format!("http://{address}/health"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return false;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        gateway.try_wait().unwrap().is_none()
            && client
                .get(format!("http://{address}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
    })
    .await
    .unwrap_or(false);
    if gateway.try_wait().unwrap().is_none() {
        gateway.kill().unwrap();
    }
    gateway.wait().unwrap();
    std::fs::remove_dir_all(base).unwrap();
    assert!(
        healthy,
        "passthrough stopped after checking a guard it never acquired"
    );
}
