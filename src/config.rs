use std::{
    collections::HashSet,
    env,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use thiserror::Error;
use uuid::Uuid;

use crate::ledger::MIN_ENTRY_MAX_BYTES;

#[derive(Clone)]
pub struct Config {
    pub listen_address: SocketAddr,
    /// Direct OpenAI-compatible endpoint used by Responses and Chat Completions.
    pub openai_base_url: String,
    pub openai_api_key: String,
    pub openai_provider: String,
    pub openai_request_max_bytes: usize,
    /// Direct Anthropic endpoint used by `/v1/messages`; never sent to clients.
    pub anthropic_base_url: String,
    pub anthropic_api_key: String,
    pub anthropic_provider: String,
    pub anthropic_request_max_bytes: usize,
    /// Maximum bytes in one decoded SSE event and in the held terminal tail.
    pub stream_event_max_bytes: usize,
    pub bootstrap_client_key: Option<String>,
    pub allowed_models: Vec<String>,
    /// `OTEL_EXPORTER_OTLP_ENDPOINT`. None means no exporter and no tracer
    /// exporter; local spans and trace ids remain available either way.
    pub otlp_endpoint: Option<String>,
    /// `DATABASE_URL`. Required: the gateway keeps its identities, policies
    /// and usage ledger here, so starting without one would mean starting
    /// something that cannot do its job.
    pub database_url: String,
    /// `DATABASE_MAX_CONNECTIONS`. Postgres has a global connection ceiling,
    /// so this is a share of it, not a throughput dial.
    pub database_max_connections: u32,
    pub admin_database_max_connections: u32,
    /// Server-side key for HMAC credential hashes. It never enters the database.
    pub credential_hash_key: Vec<u8>,
    /// Non-secret identifier persisted beside hashes so server keys can overlap.
    pub credential_hash_key_id: String,
    /// Old server-side keys accepted while credentials are being rotated.
    pub credential_retiring_hash_keys: Vec<(String, Vec<u8>)>,
    /// Lowercase environment tag embedded in newly issued keys.
    pub credential_environment: String,
    pub credential_cache_capacity: usize,
    pub credential_cache_ttl_seconds: u64,
    pub admin_bootstrap_token: Option<String>,
    pub admin_bootstrap_tenant_id: Option<Uuid>,
    pub provider_allowed_tenant_ids: Vec<Uuid>,
    pub admin_session_ttl_seconds: u64,
    pub admin_allowed_origin: Option<String>,
    pub admin_trusted_proxy_ips: Vec<IpAddr>,
    pub ledger_spool_dir: PathBuf,
    pub ledger_spool_max_bytes: u64,
    pub ledger_spool_entry_max_bytes: u64,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
    #[error("invalid LISTEN_ADDRESS: {0}")]
    InvalidAddress(#[from] std::net::AddrParseError),
    #[error("invalid DATABASE_MAX_CONNECTIONS: {0}")]
    InvalidPoolSize(#[from] std::num::ParseIntError),
    #[error("invalid ADMIN_DATABASE_MAX_CONNECTIONS: {0}")]
    InvalidAdminPoolSize(String),
    #[error("tenant ids must not be the nil UUID")]
    NilTenant,
    #[error("invalid PROVIDER_ALLOWED_TENANT_IDS entry: {0}")]
    InvalidProviderTenant(String),
    #[error("CREDENTIAL_HASH_KEY must contain at least 32 bytes")]
    WeakCredentialHashKey,
    #[error("credential hash key ids must use lowercase ASCII letters, digits or hyphens")]
    InvalidCredentialHashKeyId,
    #[error("invalid CREDENTIAL_RETIRING_HASH_KEYS entry: {0}")]
    InvalidRetiringHashKey(String),
    #[error("BOOTSTRAP_CLIENT_KEY is allowed only when CREDENTIAL_ENVIRONMENT=dev")]
    BootstrapOutsideDevelopment,
    #[error("BOOTSTRAP_CLIENT_KEY requires ENABLE_DEVELOPMENT_BOOTSTRAP=true")]
    BootstrapNotEnabled,
    #[error("ENABLE_DEVELOPMENT_BOOTSTRAP must be true or false")]
    InvalidDevelopmentBootstrapFlag,
    #[error("CREDENTIAL_ENVIRONMENT must use lowercase ASCII letters, digits or hyphens")]
    InvalidCredentialEnvironment,
    #[error("invalid CREDENTIAL_CACHE_CAPACITY: {0}")]
    InvalidCredentialCacheCapacity(String),
    #[error("invalid CREDENTIAL_CACHE_TTL_SECONDS: {0}")]
    InvalidCredentialCacheTtl(String),
    #[error("OPENAI_PROVIDER and ANTHROPIC_PROVIDER must not be empty")]
    InvalidProvider,
    #[error("invalid ledger spool capacity: {0}")]
    InvalidLedgerSpoolCapacity(String),
    #[error("invalid ANTHROPIC_REQUEST_MAX_BYTES: {0}")]
    InvalidAnthropicRequestLimit(String),
    #[error("invalid OPENAI_REQUEST_MAX_BYTES: {0}")]
    InvalidOpenAiRequestLimit(String),
    #[error("invalid STREAM_EVENT_MAX_BYTES: {0}")]
    InvalidStreamEventLimit(String),
    #[error("ADMIN_BOOTSTRAP_TOKEN and ADMIN_BOOTSTRAP_TENANT_ID must be configured together")]
    IncompleteAdminBootstrap,
    #[error("ADMIN_BOOTSTRAP_TOKEN must contain at least 32 bytes")]
    WeakAdminBootstrapToken,
    #[error("invalid ADMIN_BOOTSTRAP_TENANT_ID: {0}")]
    InvalidAdminBootstrapTenant(String),
    #[error("invalid ADMIN_SESSION_TTL_SECONDS: {0}")]
    InvalidAdminSessionTtl(String),
    #[error("ADMIN_ALLOWED_ORIGIN must be one exact http or https origin without a path")]
    InvalidAdminOrigin,
    #[error("invalid ADMIN_TRUSTED_PROXY_IPS: {0}")]
    InvalidAdminTrustedProxy(String),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let listen_address = env::var("LISTEN_ADDRESS")
            .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
            .parse()?;
        let openai_base_url = env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com".to_owned())
            .trim_end_matches('/')
            .to_owned();
        let openai_api_key = required("OPENAI_API_KEY")?;
        let openai_provider = provider_name("OPENAI_PROVIDER", "openai")?;
        let openai_request_max_bytes = env::var("OPENAI_REQUEST_MAX_BYTES")
            .unwrap_or_else(|_| (32 * 1024 * 1024).to_string())
            .parse()
            .map_err(|error: std::num::ParseIntError| {
                ConfigError::InvalidOpenAiRequestLimit(error.to_string())
            })?;
        if openai_request_max_bytes == 0 {
            return Err(ConfigError::InvalidOpenAiRequestLimit(
                "must be greater than zero".to_owned(),
            ));
        }
        let anthropic_base_url = env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".to_owned())
            .trim_end_matches('/')
            .to_owned();
        let anthropic_api_key = required("ANTHROPIC_API_KEY")?;
        let anthropic_provider = provider_name("ANTHROPIC_PROVIDER", "anthropic")?;
        let anthropic_request_max_bytes = env::var("ANTHROPIC_REQUEST_MAX_BYTES")
            .unwrap_or_else(|_| (32 * 1024 * 1024).to_string())
            .parse()
            .map_err(|error: std::num::ParseIntError| {
                ConfigError::InvalidAnthropicRequestLimit(error.to_string())
            })?;
        if anthropic_request_max_bytes == 0 {
            return Err(ConfigError::InvalidAnthropicRequestLimit(
                "must be greater than zero".to_owned(),
            ));
        }
        let stream_event_max_bytes = env::var("STREAM_EVENT_MAX_BYTES")
            .unwrap_or_else(|_| (32 * 1024 * 1024).to_string())
            .parse()
            .map_err(|error: std::num::ParseIntError| {
                ConfigError::InvalidStreamEventLimit(error.to_string())
            })?;
        if stream_event_max_bytes == 0 {
            return Err(ConfigError::InvalidStreamEventLimit(
                "must be greater than zero".to_owned(),
            ));
        }
        let allowed_models = env::var("ALLOWED_MODELS")
            .unwrap_or_else(|_| "company-default".to_owned())
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect();

        let otlp_endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty());

        let database_url = required("DATABASE_URL")?;
        let database_max_connections = env::var("DATABASE_MAX_CONNECTIONS")
            .unwrap_or_else(|_| "10".to_owned())
            .parse()?;
        let admin_database_max_connections = env::var("ADMIN_DATABASE_MAX_CONNECTIONS")
            .unwrap_or_else(|_| "2".to_owned())
            .parse::<u32>()
            .map_err(|error| ConfigError::InvalidAdminPoolSize(error.to_string()))?;
        validate_pool_budget(database_max_connections, admin_database_max_connections)?;
        let credential_environment =
            env::var("CREDENTIAL_ENVIRONMENT").unwrap_or_else(|_| "dev".to_owned());
        if !valid_tag(&credential_environment) {
            return Err(ConfigError::InvalidCredentialEnvironment);
        }
        let development_bootstrap_enabled = env::var("ENABLE_DEVELOPMENT_BOOTSTRAP")
            .unwrap_or_else(|_| "false".to_owned())
            .parse::<bool>()
            .map_err(|_| ConfigError::InvalidDevelopmentBootstrapFlag)?;
        let bootstrap_client_key = bootstrap_client_key(
            &credential_environment,
            development_bootstrap_enabled,
            env::var("BOOTSTRAP_CLIENT_KEY").ok(),
        )?;
        let credential_hash_key = required("CREDENTIAL_HASH_KEY")?.into_bytes();
        if credential_hash_key.len() < 32 {
            return Err(ConfigError::WeakCredentialHashKey);
        }
        let credential_hash_key_id =
            env::var("CREDENTIAL_HASH_KEY_ID").unwrap_or_else(|_| "v1".to_owned());
        if !valid_tag(&credential_hash_key_id) {
            return Err(ConfigError::InvalidCredentialHashKeyId);
        }
        let credential_retiring_hash_keys = retiring_hash_keys(
            env::var("CREDENTIAL_RETIRING_HASH_KEYS").unwrap_or_default(),
            &credential_hash_key_id,
        )?;
        let credential_cache_capacity = env::var("CREDENTIAL_CACHE_CAPACITY")
            .unwrap_or_else(|_| "1024".to_owned())
            .parse()
            .map_err(|error: std::num::ParseIntError| {
                ConfigError::InvalidCredentialCacheCapacity(error.to_string())
            })?;
        if credential_cache_capacity == 0 {
            return Err(ConfigError::InvalidCredentialCacheCapacity(
                "must be greater than zero".to_owned(),
            ));
        }
        let credential_cache_ttl_seconds = env::var("CREDENTIAL_CACHE_TTL_SECONDS")
            .unwrap_or_else(|_| "60".to_owned())
            .parse()
            .map_err(|error: std::num::ParseIntError| {
                ConfigError::InvalidCredentialCacheTtl(error.to_string())
            })?;
        if credential_cache_ttl_seconds == 0 {
            return Err(ConfigError::InvalidCredentialCacheTtl(
                "must be greater than zero".to_owned(),
            ));
        }
        let admin_bootstrap_token = env::var("ADMIN_BOOTSTRAP_TOKEN")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let admin_bootstrap_tenant_id = env::var("ADMIN_BOOTSTRAP_TENANT_ID")
            .ok()
            .map(|value| {
                Uuid::parse_str(value.trim())
                    .map_err(|error| ConfigError::InvalidAdminBootstrapTenant(error.to_string()))
            })
            .transpose()?;
        if admin_bootstrap_tenant_id.is_some_and(|tenant_id| tenant_id.is_nil()) {
            return Err(ConfigError::NilTenant);
        }
        if admin_bootstrap_token.is_some() != admin_bootstrap_tenant_id.is_some() {
            return Err(ConfigError::IncompleteAdminBootstrap);
        }
        let mut provider_allowed_tenant_ids =
            provider_tenant_ids(env::var("PROVIDER_ALLOWED_TENANT_IDS").unwrap_or_default())?;
        if let Some(tenant_id) = admin_bootstrap_tenant_id
            && !provider_allowed_tenant_ids.contains(&tenant_id)
        {
            provider_allowed_tenant_ids.push(tenant_id);
        }
        if bootstrap_client_key.is_some() {
            provider_allowed_tenant_ids.push(Uuid::nil());
        }
        if admin_bootstrap_token
            .as_ref()
            .is_some_and(|value| value.len() < 32)
        {
            return Err(ConfigError::WeakAdminBootstrapToken);
        }
        let admin_session_ttl_seconds = env::var("ADMIN_SESSION_TTL_SECONDS")
            .unwrap_or_else(|_| "900".to_owned())
            .parse::<u64>()
            .map_err(|error| ConfigError::InvalidAdminSessionTtl(error.to_string()))?;
        if !(60..=3600).contains(&admin_session_ttl_seconds) {
            return Err(ConfigError::InvalidAdminSessionTtl(
                "must be between 60 and 3600".to_owned(),
            ));
        }
        let admin_allowed_origin = env::var("ADMIN_ALLOWED_ORIGIN")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty());
        if admin_allowed_origin
            .as_deref()
            .is_some_and(|value| !valid_origin(value, &credential_environment))
        {
            return Err(ConfigError::InvalidAdminOrigin);
        }
        let admin_trusted_proxy_ips = env::var("ADMIN_TRUSTED_PROXY_IPS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.parse::<IpAddr>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ConfigError::InvalidAdminTrustedProxy(error.to_string()))?;
        let ledger_spool_dir = PathBuf::from(
            env::var("LEDGER_SPOOL_DIR").unwrap_or_else(|_| "var/ledger-spool".to_owned()),
        );
        let ledger_spool_max_bytes = ledger_capacity("LEDGER_SPOOL_MAX_BYTES", 64 * 1024 * 1024)?;
        let ledger_spool_entry_max_bytes =
            ledger_capacity("LEDGER_SPOOL_ENTRY_MAX_BYTES", 32 * 1024)?;
        if ledger_spool_entry_max_bytes < MIN_ENTRY_MAX_BYTES {
            return Err(ConfigError::InvalidLedgerSpoolCapacity(format!(
                "LEDGER_SPOOL_ENTRY_MAX_BYTES must be at least {MIN_ENTRY_MAX_BYTES}"
            )));
        }
        let reserved_bytes = ledger_spool_entry_max_bytes.checked_mul(3).ok_or_else(|| {
            ConfigError::InvalidLedgerSpoolCapacity(
                "LEDGER_SPOOL_ENTRY_MAX_BYTES is too large".to_owned(),
            )
        })?;
        if ledger_spool_max_bytes < reserved_bytes {
            return Err(ConfigError::InvalidLedgerSpoolCapacity(
                "LEDGER_SPOOL_MAX_BYTES must reserve three LEDGER_SPOOL_ENTRY_MAX_BYTES files"
                    .to_owned(),
            ));
        }

        Ok(Self {
            listen_address,
            openai_base_url,
            openai_api_key,
            openai_provider,
            openai_request_max_bytes,
            anthropic_base_url,
            anthropic_api_key,
            anthropic_provider,
            anthropic_request_max_bytes,
            stream_event_max_bytes,
            bootstrap_client_key,
            allowed_models,
            otlp_endpoint,
            database_url,
            database_max_connections,
            admin_database_max_connections,
            credential_hash_key,
            credential_hash_key_id,
            credential_retiring_hash_keys,
            credential_environment,
            credential_cache_capacity,
            credential_cache_ttl_seconds,
            admin_bootstrap_token,
            admin_bootstrap_tenant_id,
            provider_allowed_tenant_ids,
            admin_session_ttl_seconds,
            admin_allowed_origin,
            admin_trusted_proxy_ips,
            ledger_spool_dir,
            ledger_spool_max_bytes,
            ledger_spool_entry_max_bytes,
        })
    }

    pub fn data_database_max_connections(&self) -> u32 {
        self.database_max_connections - self.admin_database_max_connections
    }
}

fn provider_tenant_ids(value: String) -> Result<Vec<Uuid>, ConfigError> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            Uuid::parse_str(value)
                .map_err(|_| ConfigError::InvalidProviderTenant(value.to_owned()))
                .and_then(|tenant_id| {
                    (!tenant_id.is_nil())
                        .then_some(tenant_id)
                        .ok_or(ConfigError::NilTenant)
                })
        })
        .collect()
}

fn valid_origin(value: &str, environment: &str) -> bool {
    let Ok(uri) = value.parse::<axum::http::Uri>() else {
        return false;
    };
    let Some(scheme) = uri.scheme_str() else {
        return false;
    };
    let Some(authority) = uri.authority() else {
        return false;
    };
    let path_is_origin = uri.path().is_empty() || uri.path() == "/";
    let secure = scheme == "https";
    let development_loopback = environment == "dev"
        && scheme == "http"
        && matches!(authority.host(), "localhost" | "127.0.0.1" | "[::1]");
    path_is_origin
        && uri.query().is_none()
        && !authority.as_str().contains('@')
        && (secure || development_loopback)
}

fn validate_pool_budget(total: u32, admin: u32) -> Result<(), ConfigError> {
    if admin == 0 {
        return Err(ConfigError::InvalidAdminPoolSize(
            "must be greater than zero".to_owned(),
        ));
    }
    if admin >= total {
        return Err(ConfigError::InvalidAdminPoolSize(
            "must leave at least one connection for the data plane".to_owned(),
        ));
    }
    Ok(())
}

fn provider_name(variable: &'static str, default: &str) -> Result<String, ConfigError> {
    let value = env::var(variable)
        .unwrap_or_else(|_| default.to_owned())
        .trim()
        .to_owned();
    if value.is_empty() {
        Err(ConfigError::InvalidProvider)
    } else {
        Ok(value)
    }
}

fn valid_tag(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn bootstrap_client_key(
    environment: &str,
    enabled: bool,
    value: Option<String>,
) -> Result<Option<String>, ConfigError> {
    let value = value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if value.is_some() && !enabled {
        Err(ConfigError::BootstrapNotEnabled)
    } else if value.is_some() && environment != "dev" {
        Err(ConfigError::BootstrapOutsideDevelopment)
    } else {
        Ok(value)
    }
}

fn retiring_hash_keys(
    value: String,
    active_id: &str,
) -> Result<Vec<(String, Vec<u8>)>, ConfigError> {
    let mut ids = HashSet::from([active_id.to_owned()]);
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (id, secret) = entry.split_once('=').ok_or_else(|| {
                ConfigError::InvalidRetiringHashKey("expected id=secret".to_owned())
            })?;
            if !valid_tag(id) {
                return Err(ConfigError::InvalidCredentialHashKeyId);
            }
            if secret.len() < 32 {
                return Err(ConfigError::InvalidRetiringHashKey(format!(
                    "{id} secret must contain at least 32 bytes"
                )));
            }
            if !ids.insert(id.to_owned()) {
                return Err(ConfigError::InvalidRetiringHashKey(format!(
                    "duplicate key id {id}"
                )));
            }
            Ok((id.to_owned(), secret.as_bytes().to_vec()))
        })
        .collect()
}

fn required(name: &'static str) -> Result<String, ConfigError> {
    env::var(name).map_err(|_| ConfigError::Missing(name))
}

fn ledger_capacity(name: &'static str, default: u64) -> Result<u64, ConfigError> {
    let value = env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse::<u64>()
        .map_err(|error| ConfigError::InvalidLedgerSpoolCapacity(error.to_string()))?;
    if value == 0 {
        return Err(ConfigError::InvalidLedgerSpoolCapacity(format!(
            "{name} must be greater than zero"
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_is_explicitly_development_only() {
        assert!(matches!(
            bootstrap_client_key("prod", true, Some("key".to_owned())),
            Err(ConfigError::BootstrapOutsideDevelopment)
        ));
        assert!(matches!(
            bootstrap_client_key("dev", false, Some("key".to_owned())),
            Err(ConfigError::BootstrapNotEnabled)
        ));
        assert_eq!(
            bootstrap_client_key("dev", true, Some(" key ".to_owned())).unwrap(),
            Some("key".to_owned())
        );
    }

    #[test]
    fn provider_tenants_are_explicit_and_non_nil() {
        let tenant = Uuid::new_v4();
        assert_eq!(
            provider_tenant_ids(tenant.to_string()).unwrap(),
            vec![tenant]
        );
        assert!(matches!(
            provider_tenant_ids(Uuid::nil().to_string()),
            Err(ConfigError::NilTenant)
        ));
    }

    #[test]
    fn retiring_hash_key_ids_are_unique_and_secrets_are_strong() {
        let parsed =
            retiring_hash_keys("v1=old-credential-hash-key-at-least-32".to_owned(), "v2").unwrap();
        assert_eq!(parsed[0].0, "v1");
        assert!(matches!(
            retiring_hash_keys("v2=duplicate-active-hash-key-at-least-32".to_owned(), "v2"),
            Err(ConfigError::InvalidRetiringHashKey(_))
        ));
    }

    #[test]
    fn admin_pool_is_reserved_inside_the_total_database_budget() {
        validate_pool_budget(10, 2).unwrap();
        assert!(matches!(
            validate_pool_budget(10, 0),
            Err(ConfigError::InvalidAdminPoolSize(_))
        ));
        assert!(matches!(
            validate_pool_budget(2, 2),
            Err(ConfigError::InvalidAdminPoolSize(_))
        ));
    }

    #[test]
    fn admin_origin_is_exact_and_secure_outside_development() {
        assert!(valid_origin("https://admin.example.com", "prod"));
        assert!(!valid_origin("http://admin.example.com", "prod"));
        assert!(!valid_origin("https://admin.example.com/path", "prod"));
        assert!(!valid_origin("https://admin.example.com?tenant=a", "prod"));
        assert!(valid_origin("http://127.0.0.1:5173", "dev"));
        assert!(valid_origin("http://[::1]:5173", "dev"));
        assert!(!valid_origin("http://192.168.1.10:5173", "dev"));
    }
}
