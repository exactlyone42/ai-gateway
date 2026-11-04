use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use base64::Engine;
use serde::Deserialize;

use crate::error::GatewayError;

pub(crate) const BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

#[derive(Deserialize)]
struct AuthFile {
    auth_mode: String,
    tokens: Tokens,
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    account_id: String,
}

pub(crate) struct Credentials {
    pub access_token: String,
    pub account_id: String,
}

pub(crate) fn load(path: &Path, account: &str) -> Result<Credentials, GatewayError> {
    let file = std::fs::File::open(path).map_err(|_| GatewayError::ProviderUnavailable)?;
    let metadata = file
        .metadata()
        .map_err(|_| GatewayError::ProviderUnavailable)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(GatewayError::ProviderUnavailable);
    }
    let mut bytes = Vec::new();
    file.take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| GatewayError::ProviderUnavailable)?;
    if bytes.len() > 65536 {
        return Err(GatewayError::ProviderUnavailable);
    }
    let auth: AuthFile =
        serde_json::from_slice(&bytes).map_err(|_| GatewayError::ProviderUnavailable)?;
    if auth.auth_mode != "chatgpt" || auth.tokens.account_id != account || account.is_empty() {
        return Err(GatewayError::ProviderUnavailable);
    }
    let token = auth.tokens.access_token;
    if token.len() > 16384 || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(GatewayError::ProviderUnavailable);
    }
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(GatewayError::ProviderUnavailable);
    }
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| GatewayError::ProviderUnavailable)?;
    let claims: serde_json::Value =
        serde_json::from_slice(&payload).map_err(|_| GatewayError::ProviderUnavailable)?;
    let expires = claims
        .get("exp")
        .and_then(|v| v.as_i64())
        .ok_or(GatewayError::ProviderUnavailable)?;
    if expires <= chrono::Utc::now().timestamp() + 30 {
        return Err(GatewayError::ProviderUnavailable);
    }
    Ok(Credentials {
        access_token: token,
        account_id: auth.tokens.account_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, account: &str, exp: i64) {
        let claims =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#));
        let value = serde_json::json!({"auth_mode":"chatgpt","tokens":{"access_token":format!("header.{claims}.signature"),"account_id":account}});
        std::fs::write(path, value.to_string()).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn missing_created_replaced_and_invalid_credentials() {
        let root = std::env::temp_dir().join(format!("codex-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("auth.json");
        assert!(load(&path, "owner").is_err());
        let expiry = chrono::Utc::now().timestamp() + 3600;
        write(&path, "owner", expiry);
        let first = load(&path, "owner").unwrap().access_token;
        let next = root.join("next.json");
        write(&next, "owner", expiry + 100);
        std::fs::rename(next, &path).unwrap();
        assert_ne!(first, load(&path, "owner").unwrap().access_token);
        assert!(load(&path, "other").is_err());
        write(&path, "owner", 1);
        assert!(load(&path, "owner").is_err());
        write(&path, "owner", expiry);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&path, "owner").is_err());
        std::fs::write(&path, "invalid").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load(&path, "owner").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
