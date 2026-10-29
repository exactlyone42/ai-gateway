use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use uuid::Uuid;

#[test]
fn gateway_rejects_missing_capture_storage_before_database_connection() {
    let base = std::path::Path::new("/tmp").join(format!("gateway-start-check-{}", Uuid::new_v4()));
    std::fs::create_dir(&base).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let spool = base.join("spool");
    let keys = base.join("keys");
    let anchors = base.join("anchors");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_agent-pool-gateway"))
            .current_dir(&base)
            .env_clear()
            .env("DATABASE_URL", "postgres://gateway@127.0.0.1:1/gateway")
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
            .env("CONTENT_CAPTURE_ENABLED", "true")
            .env("CONTENT_CAPTURE_SPOOL_DIR", &spool)
            .env("CONTENT_CAPTURE_KEY_DIR", &keys)
            .env("CONTENT_CAPTURE_ANCHOR_DIR", &anchors)
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
    for path in [&spool, &keys, &anchors] {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let no_key = run();
    assert!(!no_key.status.success());
    assert!(String::from_utf8_lossy(&no_key.stderr).contains("capture keyring unavailable"));
}
