use std::{
    collections::HashSet,
    env,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use thiserror::Error;
use uuid::Uuid;

use crate::ledger::MIN_ENTRY_MAX_BYTES;
use crate::model_registry::{GatewayOperation, ProviderKind};
use crate::routing::RoutingConfig;
use crate::scheduler::SchedulerConfig;

#[derive(Clone)]
pub struct ExtraProviderProfile {
    pub kind: ProviderKind,
    pub credential_ref: String,
    pub provider_key: String,
    pub base_url: String,
    pub operations: Vec<GatewayOperation>,
    pub secret: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExtraProviderProfile {
    kind: String,
    credential_ref: String,
    provider_key: String,
    base_url: String,
    operations: Vec<String>,
    secret_env: Option<String>,
}

#[derive(Clone)]
pub struct Config {
    pub listen_address: SocketAddr,
    /// Direct OpenAI-compatible endpoint used by Responses and Chat Completions.
    pub openai_base_url: String,
    pub openai_api_key: String,
    pub codex_auth_file: Option<std::path::PathBuf>,
    pub codex_account_id: String,
    pub codex_owner_user_id: Option<Uuid>,
    pub openai_provider: String,
    pub openai_request_max_bytes: usize,
    /// Direct Anthropic endpoint used by `/v1/messages`; never sent to clients.
    pub anthropic_base_url: String,
    pub anthropic_api_key: String,
    pub anthropic_auth_token: Option<String>,
    pub anthropic_subscription_token_file: Option<std::path::PathBuf>,
    pub anthropic_personal_token_dir: Option<std::path::PathBuf>,
    pub anthropic_subscription_token_seed: Option<String>,
    pub anthropic_client_oauth: bool,
    pub anthropic_oauth_credential_id: Option<Uuid>,
    pub anthropic_provider: String,
    pub anthropic_request_max_bytes: usize,
    pub extra_provider_profiles: Vec<ExtraProviderProfile>,
    /// Maximum bytes in one decoded SSE event and in the held terminal tail.
    pub stream_event_max_bytes: usize,
    /// Maximum silence visible to a streaming client while the provider is
    /// still preparing or producing its response.
    pub stream_heartbeat_interval: Duration,
    /// Maximum silence between provider response reads. This does not cap the
    /// total lifetime of an active stream.
    pub provider_idle_timeout: Duration,
    pub local_provider_idle_timeout: Duration,
    pub provider_connect_timeout: Duration,
    pub request_header_max_bytes: usize,
    pub drain_timeout: Duration,
    pub shutdown_finalize_timeout: Duration,
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
    pub policy_cache_capacity: usize,
    pub policy_cache_ttl_seconds: u64,
    pub admin_bootstrap_token: Option<String>,
    pub admin_bootstrap_tenant_id: Option<Uuid>,
    pub provider_allowed_tenant_ids: Vec<Uuid>,
    pub admin_session_ttl_seconds: u64,
    pub admin_allowed_origin: Option<String>,
    pub admin_trusted_proxy_ips: Vec<IpAddr>,
    pub ledger_spool_dir: PathBuf,
    pub ledger_spool_max_bytes: u64,
    pub ledger_spool_entry_max_bytes: u64,
    pub content_capture_enabled: bool,
    pub content_capture_spool_dir: PathBuf,
    pub content_capture_key_dir: PathBuf,
    pub content_capture_anchor_dir: PathBuf,
    pub content_capture_erasure_dir: PathBuf,
    pub content_capture_witness_dir: PathBuf,
    pub content_capture_remote_witness_url: Option<String>,
    pub content_capture_test_allow_local_witness: bool,
    pub routing: RoutingConfig,
    pub jev: Option<crate::jev::JevConfig>,
    pub scheduler: SchedulerConfig,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("CONTENT_CAPTURE_ENABLED must be true or false")]
    InvalidContentCaptureEnabled,
    #[error("CODEX_OWNER_USER_ID 必須是有效的使用者識別碼（UUID）")]
    InvalidCodexOwner,
    #[error("invalid Jev configuration")]
    InvalidJev,
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
    #[error("invalid POLICY_CACHE_CAPACITY: {0}")]
    InvalidPolicyCacheCapacity(String),
    #[error("invalid POLICY_CACHE_TTL_SECONDS: {0}")]
    InvalidPolicyCacheTtl(String),
    #[error("OPENAI_PROVIDER and ANTHROPIC_PROVIDER must not be empty")]
    InvalidProvider,
    #[error("invalid EXTRA_PROVIDER_PROFILES_JSON: {0}")]
    InvalidExtraProviderProfiles(String),
    #[error("invalid ledger spool capacity: {0}")]
    InvalidLedgerSpoolCapacity(String),
    #[error("invalid ANTHROPIC_REQUEST_MAX_BYTES: {0}")]
    InvalidAnthropicRequestLimit(String),
    #[error("ANTHROPIC_CLIENT_OAUTH and ANTHROPIC_OAUTH_CREDENTIAL_ID must be configured together")]
    IncompleteAnthropicClientOauth,
    #[error("ANTHROPIC_SUBSCRIPTION_TOKEN_FILE cannot be combined with ANTHROPIC_CLIENT_OAUTH")]
    ConflictingAnthropicSubscription,
    #[error("invalid Anthropic client OAuth configuration: {0}")]
    InvalidAnthropicClientOauth(String),
    #[error("invalid OPENAI_REQUEST_MAX_BYTES: {0}")]
    InvalidOpenAiRequestLimit(String),
    #[error("invalid STREAM_EVENT_MAX_BYTES: {0}")]
    InvalidStreamEventLimit(String),
    #[error("invalid STREAM_HEARTBEAT_INTERVAL_MS: {0}")]
    InvalidStreamHeartbeatInterval(String),
    #[error("invalid PROVIDER_IDLE_TIMEOUT_MS: {0}")]
    InvalidProviderIdleTimeout(String),
    #[error("invalid LOCAL_PROVIDER_IDLE_TIMEOUT_MS: {0}")]
    InvalidLocalProviderIdleTimeout(String),
    #[error("invalid PROVIDER_CONNECT_TIMEOUT_MS: {0}")]
    InvalidProviderConnectTimeout(String),
    #[error("invalid REQUEST_HEADER_MAX_BYTES: {0}")]
    InvalidRequestHeaderLimit(String),
    #[error("invalid DRAIN_TIMEOUT_MS: {0}")]
    InvalidDrainTimeout(String),
    #[error("invalid SHUTDOWN_FINALIZE_TIMEOUT_MS: {0}")]
    InvalidShutdownFinalizeTimeout(String),
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
    #[error(
        "no tenant may use the providers: set PROVIDER_ALLOWED_TENANT_IDS, or ADMIN_BOOTSTRAP_TENANT_ID with ADMIN_BOOTSTRAP_TOKEN"
    )]
    NoAllowedTenant,
    #[error(
        "{0} still holds a value that ships in this repository; set a real secret or run with CREDENTIAL_ENVIRONMENT=dev"
    )]
    DevelopmentSecretOutsideDevelopment(&'static str),
    #[error("invalid scheduler setting: {0}")]
    InvalidScheduler(String),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let listen_address = setting("LISTEN_ADDRESS")
            .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
            .parse()?;
        let openai_base_url = setting("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com".to_owned())
            .trim_end_matches('/')
            .to_owned();
        let codex_auth_file = setting("CODEX_AUTH_FILE")
            .ok()
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from);
        let codex_account_id = setting("CODEX_ACCOUNT_ID").unwrap_or_default();
        let codex_owner_user_id = setting("CODEX_OWNER_USER_ID")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| value.parse().map_err(|_| ConfigError::InvalidCodexOwner))
            .transpose()?;
        if codex_auth_file.is_some() && codex_owner_user_id.is_none() {
            return Err(ConfigError::InvalidCodexOwner);
        }
        let openai_api_key = setting("OPENAI_API_KEY").unwrap_or_default();
        let openai_provider = provider_name("OPENAI_PROVIDER", "openai")?;
        let openai_request_max_bytes = setting("OPENAI_REQUEST_MAX_BYTES")
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
        let anthropic_base_url = setting("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".to_owned())
            .trim_end_matches('/')
            .to_owned();
        let anthropic_client_oauth = setting("ANTHROPIC_CLIENT_OAUTH")
            .unwrap_or_else(|_| "false".to_owned())
            .parse::<bool>()
            .map_err(|error| ConfigError::InvalidAnthropicClientOauth(error.to_string()))?;
        let anthropic_oauth_credential_id = setting("ANTHROPIC_OAUTH_CREDENTIAL_ID")
            .ok()
            .map(|value| Uuid::parse_str(&value))
            .transpose()
            .map_err(|error| ConfigError::InvalidAnthropicClientOauth(error.to_string()))?;
        if anthropic_client_oauth != anthropic_oauth_credential_id.is_some() {
            return Err(ConfigError::IncompleteAnthropicClientOauth);
        }
        let anthropic_auth_token = setting("ANTHROPIC_AUTH_TOKEN").ok();
        let anthropic_subscription_token_file = setting("ANTHROPIC_SUBSCRIPTION_TOKEN_FILE")
            .ok()
            .map(std::path::PathBuf::from);
        let anthropic_subscription_token_seed = setting("ANTHROPIC_SUBSCRIPTION_TOKEN").ok();
        let anthropic_personal_token_dir = setting("ANTHROPIC_PERSONAL_TOKEN_DIR")
            .ok()
            .map(PathBuf::from);
        if anthropic_client_oauth
            && (anthropic_subscription_token_file.is_some()
                || anthropic_personal_token_dir.is_some())
        {
            return Err(ConfigError::ConflictingAnthropicSubscription);
        }
        let anthropic_api_key = setting("ANTHROPIC_API_KEY").unwrap_or_default();
        let anthropic_provider = provider_name("ANTHROPIC_PROVIDER", "anthropic")?;
        let anthropic_request_max_bytes = setting("ANTHROPIC_REQUEST_MAX_BYTES")
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
        let extra_provider_profiles = extra_provider_profiles(
            setting("EXTRA_PROVIDER_PROFILES_JSON").unwrap_or_else(|_| "[]".to_owned()),
            anthropic_personal_token_dir.is_some(),
        )?;
        let stream_event_max_bytes = setting("STREAM_EVENT_MAX_BYTES")
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
        let provider_idle_timeout_ms = setting("PROVIDER_IDLE_TIMEOUT_MS")
            .unwrap_or_else(|_| "120000".to_owned())
            .parse::<u64>()
            .map_err(|error| ConfigError::InvalidProviderIdleTimeout(error.to_string()))?;
        if provider_idle_timeout_ms == 0 || provider_idle_timeout_ms > 600_000 {
            return Err(ConfigError::InvalidProviderIdleTimeout(
                "must be between 1 and 600000".to_owned(),
            ));
        }
        let provider_idle_timeout = Duration::from_millis(provider_idle_timeout_ms);
        let local_provider_idle_timeout_ms = setting("LOCAL_PROVIDER_IDLE_TIMEOUT_MS")
            .unwrap_or_else(|_| "300000".to_owned())
            .parse::<u64>()
            .map_err(|error| ConfigError::InvalidLocalProviderIdleTimeout(error.to_string()))?;
        if !(1..=600_000).contains(&local_provider_idle_timeout_ms) {
            return Err(ConfigError::InvalidLocalProviderIdleTimeout(
                "must be between 1 and 600000".to_owned(),
            ));
        }
        let local_provider_idle_timeout = Duration::from_millis(local_provider_idle_timeout_ms);
        let stream_heartbeat_interval_ms = setting("STREAM_HEARTBEAT_INTERVAL_MS")
            .unwrap_or_else(|_| "30000".to_owned())
            .parse::<u64>()
            .map_err(|error| ConfigError::InvalidStreamHeartbeatInterval(error.to_string()))?;
        if stream_heartbeat_interval_ms == 0 || stream_heartbeat_interval_ms >= 60_000 {
            return Err(ConfigError::InvalidStreamHeartbeatInterval(
                "must be between 1 and 59999".to_owned(),
            ));
        }
        let stream_heartbeat_interval = Duration::from_millis(stream_heartbeat_interval_ms);
        let provider_connect_timeout_ms = setting("PROVIDER_CONNECT_TIMEOUT_MS")
            .unwrap_or_else(|_| "10000".to_owned())
            .parse::<u64>()
            .map_err(|error| ConfigError::InvalidProviderConnectTimeout(error.to_string()))?;
        if !(1..=120_000).contains(&provider_connect_timeout_ms) {
            return Err(ConfigError::InvalidProviderConnectTimeout(
                "must be between 1 and 120000".to_owned(),
            ));
        }
        let provider_connect_timeout = Duration::from_millis(provider_connect_timeout_ms);
        let request_header_max_bytes = setting("REQUEST_HEADER_MAX_BYTES")
            .unwrap_or_else(|_| (64 * 1024).to_string())
            .parse::<usize>()
            .map_err(|error| ConfigError::InvalidRequestHeaderLimit(error.to_string()))?;
        if !(8192..=256 * 1024).contains(&request_header_max_bytes) {
            return Err(ConfigError::InvalidRequestHeaderLimit(
                "must be between 8192 and 262144".to_owned(),
            ));
        }
        let drain_timeout_ms = setting("DRAIN_TIMEOUT_MS")
            .unwrap_or_else(|_| "30000".to_owned())
            .parse::<u64>()
            .map_err(|error| ConfigError::InvalidDrainTimeout(error.to_string()))?;
        if drain_timeout_ms > 600_000 {
            return Err(ConfigError::InvalidDrainTimeout(
                "must be between 0 and 600000".to_owned(),
            ));
        }
        let drain_timeout = Duration::from_millis(drain_timeout_ms);
        let shutdown_finalize_timeout_ms = setting("SHUTDOWN_FINALIZE_TIMEOUT_MS")
            .unwrap_or_else(|_| "5000".to_owned())
            .parse::<u64>()
            .map_err(|error| ConfigError::InvalidShutdownFinalizeTimeout(error.to_string()))?;
        if !(1..=60_000).contains(&shutdown_finalize_timeout_ms) {
            return Err(ConfigError::InvalidShutdownFinalizeTimeout(
                "must be between 1 and 60000".to_owned(),
            ));
        }
        let shutdown_finalize_timeout = Duration::from_millis(shutdown_finalize_timeout_ms);
        // raw-env: an explicitly empty list means "allow nothing".
        let allowed_models = model_allowlist(env::var("ALLOWED_MODELS").ok());

        let otlp_endpoint = setting("OTEL_EXPORTER_OTLP_ENDPOINT")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty());

        let database_url = required("DATABASE_URL")?;
        let database_max_connections = setting("DATABASE_MAX_CONNECTIONS")
            .unwrap_or_else(|_| "10".to_owned())
            .parse()?;
        let admin_database_max_connections = setting("ADMIN_DATABASE_MAX_CONNECTIONS")
            .unwrap_or_else(|_| "2".to_owned())
            .parse::<u32>()
            .map_err(|error| ConfigError::InvalidAdminPoolSize(error.to_string()))?;
        validate_pool_budget(database_max_connections, admin_database_max_connections)?;
        let credential_environment =
            setting("CREDENTIAL_ENVIRONMENT").unwrap_or_else(|_| "dev".to_owned());
        if !valid_tag(&credential_environment) {
            return Err(ConfigError::InvalidCredentialEnvironment);
        }
        let development_bootstrap_enabled = setting("ENABLE_DEVELOPMENT_BOOTSTRAP")
            .unwrap_or_else(|_| "false".to_owned())
            .parse::<bool>()
            .map_err(|_| ConfigError::InvalidDevelopmentBootstrapFlag)?;
        let bootstrap_client_key = bootstrap_client_key(
            &credential_environment,
            development_bootstrap_enabled,
            setting("BOOTSTRAP_CLIENT_KEY").ok(),
        )?;
        let credential_hash_key = required("CREDENTIAL_HASH_KEY")?.into_bytes();
        if credential_hash_key.len() < 32 {
            return Err(ConfigError::WeakCredentialHashKey);
        }
        let credential_hash_key_id =
            setting("CREDENTIAL_HASH_KEY_ID").unwrap_or_else(|_| "v1".to_owned());
        if !valid_tag(&credential_hash_key_id) {
            return Err(ConfigError::InvalidCredentialHashKeyId);
        }
        let credential_retiring_hash_keys = retiring_hash_keys(
            setting("CREDENTIAL_RETIRING_HASH_KEYS").unwrap_or_default(),
            &credential_hash_key_id,
        )?;
        let credential_cache_capacity = setting("CREDENTIAL_CACHE_CAPACITY")
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
        let credential_cache_ttl_seconds = setting("CREDENTIAL_CACHE_TTL_SECONDS")
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
        let policy_cache_capacity = setting("POLICY_CACHE_CAPACITY")
            .unwrap_or_else(|_| "1024".to_owned())
            .parse()
            .map_err(|error: std::num::ParseIntError| {
                ConfigError::InvalidPolicyCacheCapacity(error.to_string())
            })?;
        if policy_cache_capacity == 0 {
            return Err(ConfigError::InvalidPolicyCacheCapacity(
                "must be greater than zero".to_owned(),
            ));
        }
        let policy_cache_ttl_seconds = setting("POLICY_CACHE_TTL_SECONDS")
            .unwrap_or_else(|_| "60".to_owned())
            .parse()
            .map_err(|error: std::num::ParseIntError| {
                ConfigError::InvalidPolicyCacheTtl(error.to_string())
            })?;
        if policy_cache_ttl_seconds == 0 {
            return Err(ConfigError::InvalidPolicyCacheTtl(
                "must be greater than zero".to_owned(),
            ));
        }
        let admin_bootstrap_token = setting("ADMIN_BOOTSTRAP_TOKEN")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let admin_bootstrap_tenant_id =
            admin_bootstrap_tenant_id(setting("ADMIN_BOOTSTRAP_TENANT_ID").ok())?;
        if admin_bootstrap_token.is_some() != admin_bootstrap_tenant_id.is_some() {
            return Err(ConfigError::IncompleteAdminBootstrap);
        }
        reject_placeholder_secret(
            &credential_environment,
            "CREDENTIAL_HASH_KEY",
            &String::from_utf8_lossy(&credential_hash_key),
        )?;
        reject_placeholder_secret(&credential_environment, "OPENAI_API_KEY", &openai_api_key)?;
        if anthropic_auth_token.is_none() {
            reject_placeholder_secret(
                &credential_environment,
                "ANTHROPIC_API_KEY",
                &anthropic_api_key,
            )?;
        }
        let provider_allowed_tenant_ids = provider_allowed_tenants(
            setting("PROVIDER_ALLOWED_TENANT_IDS").unwrap_or_default(),
            admin_bootstrap_tenant_id,
            bootstrap_client_key.is_some(),
        )?;
        if admin_bootstrap_token
            .as_ref()
            .is_some_and(|value| value.len() < 32)
        {
            return Err(ConfigError::WeakAdminBootstrapToken);
        }
        let admin_session_ttl_seconds = setting("ADMIN_SESSION_TTL_SECONDS")
            .unwrap_or_else(|_| "900".to_owned())
            .parse::<u64>()
            .map_err(|error| ConfigError::InvalidAdminSessionTtl(error.to_string()))?;
        if !(60..=3600).contains(&admin_session_ttl_seconds) {
            return Err(ConfigError::InvalidAdminSessionTtl(
                "must be between 60 and 3600".to_owned(),
            ));
        }
        let admin_allowed_origin = setting("ADMIN_ALLOWED_ORIGIN")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty());
        if admin_allowed_origin
            .as_deref()
            .is_some_and(|value| !valid_origin(value, &credential_environment))
        {
            return Err(ConfigError::InvalidAdminOrigin);
        }
        let admin_trusted_proxy_ips = setting("ADMIN_TRUSTED_PROXY_IPS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.parse::<IpAddr>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ConfigError::InvalidAdminTrustedProxy(error.to_string()))?;
        let ledger_spool_dir = PathBuf::from(
            setting("LEDGER_SPOOL_DIR").unwrap_or_else(|_| "var/ledger-spool".to_owned()),
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
        let content_capture_enabled =
            parse_content_capture_enabled(setting("CONTENT_CAPTURE_ENABLED").ok().as_deref())?;
        let content_capture_spool_dir = PathBuf::from(
            setting("CONTENT_CAPTURE_SPOOL_DIR")
                .unwrap_or_else(|_| "/var/lib/agent-pool-gateway/capture-spool".to_owned()),
        );
        let content_capture_key_dir = PathBuf::from(
            setting("CONTENT_CAPTURE_KEY_DIR")
                .unwrap_or_else(|_| "/var/lib/agent-pool-gateway/capture-keys".to_owned()),
        );
        let content_capture_anchor_dir = PathBuf::from(
            setting("CONTENT_CAPTURE_ANCHOR_DIR")
                .unwrap_or_else(|_| "/var/lib/agent-pool-gateway/capture-anchors".to_owned()),
        );
        let (content_capture_erasure_dir, content_capture_witness_dir) = capture_erasure_paths();
        let content_capture_remote_witness_url = setting("CONTENT_CAPTURE_REMOTE_WITNESS_URL").ok();
        let content_capture_test_allow_local_witness = cfg!(debug_assertions)
            && credential_environment == "dev"
            && setting("CONTENT_CAPTURE_TEST_ALLOW_LOCAL_WITNESS").as_deref() == Ok("1");
        let scheduler = scheduler_config()?;
        validate_capture_pool_budget(
            database_max_connections,
            admin_database_max_connections,
            content_capture_enabled,
            scheduler.passthrough,
        )?;
        let routing = routing_config()?;

        Ok(Self {
            listen_address,
            openai_base_url,
            openai_api_key,
            codex_auth_file,
            codex_account_id,
            codex_owner_user_id,
            openai_provider,
            openai_request_max_bytes,
            anthropic_base_url,
            anthropic_api_key,
            anthropic_auth_token,
            anthropic_subscription_token_file,
            anthropic_personal_token_dir,
            anthropic_subscription_token_seed,
            anthropic_client_oauth,
            anthropic_oauth_credential_id,
            anthropic_provider,
            anthropic_request_max_bytes,
            extra_provider_profiles,
            stream_event_max_bytes,
            stream_heartbeat_interval,
            provider_idle_timeout,
            local_provider_idle_timeout,
            provider_connect_timeout,
            request_header_max_bytes,
            drain_timeout,
            shutdown_finalize_timeout,
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
            policy_cache_capacity,
            policy_cache_ttl_seconds,
            admin_bootstrap_token,
            admin_bootstrap_tenant_id,
            provider_allowed_tenant_ids,
            admin_session_ttl_seconds,
            admin_allowed_origin,
            admin_trusted_proxy_ips,
            ledger_spool_dir,
            ledger_spool_max_bytes,
            ledger_spool_entry_max_bytes,
            content_capture_enabled,
            content_capture_spool_dir,
            content_capture_key_dir,
            content_capture_anchor_dir,
            content_capture_erasure_dir,
            content_capture_witness_dir,
            content_capture_remote_witness_url,
            content_capture_test_allow_local_witness,
            jev: load_jev()?,
            routing,
            scheduler,
        })
    }

    pub fn data_database_max_connections(&self) -> u32 {
        self.database_max_connections - self.admin_database_max_connections
    }

    pub fn transport_header_max_bytes(&self) -> usize {
        self.request_header_max_bytes.saturating_mul(2)
    }
}

pub(crate) fn capture_erasure_paths() -> (PathBuf, PathBuf) {
    (
        PathBuf::from(
            setting("CONTENT_CAPTURE_ERASURE_DIR")
                .unwrap_or_else(|_| "/var/lib/agent-pool-gateway/capture-erasure".to_owned()),
        ),
        PathBuf::from(
            setting("CONTENT_CAPTURE_WITNESS_DIR")
                .unwrap_or_else(|_| "/var/lib/agent-pool-gateway/capture-witness".to_owned()),
        ),
    )
}

fn parse_content_capture_enabled(value: Option<&str>) -> Result<bool, ConfigError> {
    match value {
        None | Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(_) => Err(ConfigError::InvalidContentCaptureEnabled),
    }
}

fn routing_config() -> Result<RoutingConfig, ConfigError> {
    let defaults = RoutingConfig::default();
    Ok(RoutingConfig {
        health_aware: scheduler_bool("ROUTING_HEALTH_AWARE", defaults.health_aware)?,
        load_aware: scheduler_bool("ROUTING_LOAD_AWARE", defaults.load_aware)?,
        cost_aware: scheduler_bool("ROUTING_COST_AWARE", defaults.cost_aware)?,
        quota_aware: scheduler_bool("ROUTING_QUOTA_AWARE", defaults.quota_aware)?,
        auto_quality: scheduler_bool("ROUTING_AUTO_QUALITY", defaults.auto_quality)?,
    })
}

fn extra_provider_profiles(
    value: String,
    personal: bool,
) -> Result<Vec<ExtraProviderProfile>, ConfigError> {
    let raw = serde_json::from_str::<Vec<RawExtraProviderProfile>>(&value)
        .map_err(|error| ConfigError::InvalidExtraProviderProfiles(error.to_string()))?;
    raw.into_iter()
        .map(|profile| {
            let kind = match profile.kind.as_str() {
                "anthropic" if personal => ProviderKind::Anthropic,
                "openai" if personal => ProviderKind::OpenAi,
                "gemini" => ProviderKind::Gemini,
                "xai" => ProviderKind::Xai,
                "vllm" => ProviderKind::Vllm,
                "ollama" => ProviderKind::Ollama,
                _ => {
                    return Err(ConfigError::InvalidExtraProviderProfiles(format!(
                        "unsupported kind {}",
                        profile.kind
                    )));
                }
            };
            let operations = profile
                .operations
                .iter()
                .map(|operation| {
                    GatewayOperation::parse(operation).ok_or_else(|| {
                        ConfigError::InvalidExtraProviderProfiles(format!(
                            "unsupported operation {operation}"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let valid_operations = !operations.is_empty()
                && operations.iter().all(|operation| match kind {
                    ProviderKind::Gemini => *operation == GatewayOperation::GeminiGenerateContent,
                    ProviderKind::Ollama if operations == [GatewayOperation::AnthropicMessages] => {
                        true
                    }
                    ProviderKind::Xai | ProviderKind::Vllm | ProviderKind::Ollama => matches!(
                        operation,
                        GatewayOperation::OpenAiResponses | GatewayOperation::OpenAiChatCompletions
                    ),
                    ProviderKind::Anthropic => matches!(
                        operation,
                        GatewayOperation::AnthropicMessages
                            | GatewayOperation::AnthropicCountTokens
                    ),
                    ProviderKind::OpenAi => matches!(
                        operation,
                        GatewayOperation::OpenAiResponses | GatewayOperation::OpenAiChatCompletions
                    ),
                });
            if !valid_operations
                || profile.credential_ref.trim().is_empty()
                || profile.provider_key.trim().is_empty()
                || profile.base_url.trim().is_empty()
            {
                return Err(ConfigError::InvalidExtraProviderProfiles(
                    "profile fields or operations are invalid".to_owned(),
                ));
            }
            let secret = profile
                .secret_env
                .as_deref()
                .map(|name| {
                    if name.trim().is_empty() {
                        return Err(ConfigError::InvalidExtraProviderProfiles(
                            "secret_env must not be empty".to_owned(),
                        ));
                    }
                    setting(name).map_err(|_| {
                        ConfigError::InvalidExtraProviderProfiles(format!(
                            "missing secret environment variable {name}"
                        ))
                    })
                })
                .transpose()?;
            if matches!(kind, ProviderKind::Gemini | ProviderKind::Xai)
                && secret.is_none()
                && !personal
            {
                return Err(ConfigError::InvalidExtraProviderProfiles(
                    "gemini and xai require secret_env".to_owned(),
                ));
            }
            if matches!(kind, ProviderKind::Anthropic | ProviderKind::OpenAi) && secret.is_some() {
                return Err(ConfigError::InvalidExtraProviderProfiles(
                    "personal provider profiles must not set secret_env".to_owned(),
                ));
            }
            Ok(ExtraProviderProfile {
                kind,
                credential_ref: profile.credential_ref,
                provider_key: profile.provider_key,
                base_url: profile.base_url.trim_end_matches('/').to_owned(),
                operations,
                secret,
            })
        })
        .collect()
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

fn validate_capture_pool_budget(
    total: u32,
    admin: u32,
    capture: bool,
    passthrough: bool,
) -> Result<(), ConfigError> {
    let minimum = if passthrough { 3 } else { 4 };
    if capture && total.saturating_sub(admin) < minimum {
        return Err(ConfigError::InvalidAdminPoolSize(format!(
            "content capture requires at least {minimum} data-plane connections"
        )));
    }
    Ok(())
}

fn validate_pool_budget(total: u32, admin: u32) -> Result<(), ConfigError> {
    if admin == 0 {
        return Err(ConfigError::InvalidAdminPoolSize(
            "must be greater than zero".to_owned(),
        ));
    }
    if total.saturating_sub(admin) < 2 {
        return Err(ConfigError::InvalidAdminPoolSize(
            "must leave one quota guard connection and one serving connection for the data plane"
                .to_owned(),
        ));
    }
    Ok(())
}

fn provider_name(variable: &'static str, default: &str) -> Result<String, ConfigError> {
    let value = setting(variable)
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

/// Placeholder secrets that ship in `deploy/docker-compose.yml` and
/// `.env.example`. Anyone who can read the repository knows them, so a
/// credential hash key taken from this list makes every credential fingerprint
/// predictable. They are fine for development and must never reach an
/// environment that calls itself anything else.
const REPOSITORY_PLACEHOLDER_SECRETS: [&str; 3] = [
    "development-only-credential-hash-key",
    "replace-with-at-least-32-random-bytes",
    "replace-me",
];

fn reject_placeholder_secret(
    environment: &str,
    name: &'static str,
    value: &str,
) -> Result<(), ConfigError> {
    if environment == "dev" {
        return Ok(());
    }
    if REPOSITORY_PLACEHOLDER_SECRETS.contains(&value.trim()) {
        return Err(ConfigError::DevelopmentSecretOutsideDevelopment(name));
    }
    Ok(())
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

/// Collects every tenant that may use the server-side provider credentials:
/// the operator's own list, the admin bootstrap tenant, and the nil tenant that
/// belongs to the development bootstrap key.
///
/// Fails closed when the result is empty, and says which setting is missing.
/// An empty list still builds provider profiles, which the registry then
/// rejects with an error that names no environment variable at all.
fn provider_allowed_tenants(
    configured: String,
    bootstrap_tenant: Option<Uuid>,
    has_bootstrap_client_key: bool,
) -> Result<Vec<Uuid>, ConfigError> {
    let mut tenants = provider_tenant_ids(configured)?;
    if let Some(tenant_id) = bootstrap_tenant
        && !tenants.contains(&tenant_id)
    {
        tenants.push(tenant_id);
    }
    if has_bootstrap_client_key {
        tenants.push(Uuid::nil());
    }
    if tenants.is_empty() {
        return Err(ConfigError::NoAllowedTenant);
    }
    Ok(tenants)
}

/// An empty value means "not set", the same as `ADMIN_BOOTSTRAP_TOKEN`. A
/// deployment template that forwards both variables unconditionally would
/// otherwise refuse to start instead of running without admin bootstrap.
fn admin_bootstrap_tenant_id(value: Option<String>) -> Result<Option<Uuid>, ConfigError> {
    let Some(value) = value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let tenant_id = Uuid::parse_str(&value)
        .map_err(|error| ConfigError::InvalidAdminBootstrapTenant(error.to_string()))?;
    if tenant_id.is_nil() {
        return Err(ConfigError::NilTenant);
    }
    Ok(Some(tenant_id))
}

/// Reads a setting, treating an empty or blank value as "not set".
///
/// A deployment template can then forward every variable unconditionally,
/// instead of leaving them out or copying this file's defaults -- the two
/// places then drift apart, and a template that forwards a variable it has no
/// value for hands the process an empty string it has to reject.
/// The development model allowlist.
///
/// This is the one setting where empty is a decision, not an omission: an
/// explicitly empty list allows nothing, which is not what leaving the variable
/// out means. So it reads the raw value instead of going through [`setting`].
fn model_allowlist(raw: Option<String>) -> Vec<String> {
    raw.unwrap_or_else(|| "company-default".to_owned())
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// The value itself is returned untouched: a secret may legitimately carry
/// surrounding whitespace, and trimming it would silently change the key.
fn setting(name: &str) -> Result<String, env::VarError> {
    // raw-env: this is the wrapper every other read goes through.
    match env::var(name) {
        Ok(value) if value.trim().is_empty() => Err(env::VarError::NotPresent),
        other => other,
    }
}

fn required(name: &'static str) -> Result<String, ConfigError> {
    setting(name).map_err(|_| ConfigError::Missing(name))
}

fn ledger_capacity(name: &'static str, default: u64) -> Result<u64, ConfigError> {
    let value = setting(name)
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

fn scheduler_config() -> Result<SchedulerConfig, ConfigError> {
    let defaults = SchedulerConfig::default();
    let config = SchedulerConfig {
        passthrough: scheduler_bool("SCHEDULER_PASSTHROUGH", defaults.passthrough)?,
        active_interactive_reserved: scheduler_usize(
            "SCHEDULER_ACTIVE_INTERACTIVE_RESERVED",
            defaults.active_interactive_reserved,
        )?,
        active_shared: scheduler_usize("SCHEDULER_ACTIVE_SHARED", defaults.active_shared)?,
        active_background_reserved: scheduler_usize(
            "SCHEDULER_ACTIVE_BACKGROUND_RESERVED",
            defaults.active_background_reserved,
        )?,
        queue_interactive_reserved: scheduler_usize(
            "SCHEDULER_QUEUE_INTERACTIVE_RESERVED",
            defaults.queue_interactive_reserved,
        )?,
        queue_shared: scheduler_usize("SCHEDULER_QUEUE_SHARED", defaults.queue_shared)?,
        queue_background_reserved: scheduler_usize(
            "SCHEDULER_QUEUE_BACKGROUND_RESERVED",
            defaults.queue_background_reserved,
        )?,
        queue_per_tenant: scheduler_usize("SCHEDULER_QUEUE_PER_TENANT", defaults.queue_per_tenant)?,
        queue_per_credential: scheduler_usize(
            "SCHEDULER_QUEUE_PER_CREDENTIAL",
            defaults.queue_per_credential,
        )?,
        per_provider: scheduler_usize("SCHEDULER_PER_PROVIDER", defaults.per_provider)?,
        per_model: scheduler_usize("SCHEDULER_PER_MODEL", defaults.per_model)?,
        per_user: scheduler_usize("SCHEDULER_PER_USER", defaults.per_user)?,
        per_device: scheduler_usize("SCHEDULER_PER_DEVICE", defaults.per_device)?,
        per_client: scheduler_usize("SCHEDULER_PER_CLIENT", defaults.per_client)?,
        per_agent: scheduler_usize("SCHEDULER_PER_AGENT", defaults.per_agent)?,
        per_credential: scheduler_usize("SCHEDULER_PER_CREDENTIAL", defaults.per_credential)?,
        per_workload: scheduler_usize("SCHEDULER_PER_WORKLOAD", defaults.per_workload)?,
        hard_queue_timeout: Duration::from_millis(scheduler_u64(
            "SCHEDULER_HARD_QUEUE_TIMEOUT_MS",
            defaults.hard_queue_timeout.as_millis() as u64,
        )?),
        aging_interval: Duration::from_millis(scheduler_u64(
            "SCHEDULER_AGING_INTERVAL_MS",
            defaults.aging_interval.as_millis() as u64,
        )?),
        aging_enabled: scheduler_bool("SCHEDULER_AGING_ENABLED", defaults.aging_enabled)?,
        reservations_enabled: scheduler_bool(
            "SCHEDULER_RESERVATIONS_ENABLED",
            defaults.reservations_enabled,
        )?,
    };
    config
        .validate()
        .map_err(|error| ConfigError::InvalidScheduler(error.to_string()))?;
    Ok(config)
}

fn scheduler_usize(name: &'static str, default: usize) -> Result<usize, ConfigError> {
    let value = scheduler_u64(name, default as u64)?;
    usize::try_from(value)
        .map_err(|_| ConfigError::InvalidScheduler(format!("{name} is too large")))
}

fn scheduler_u64(name: &'static str, default: u64) -> Result<u64, ConfigError> {
    let value = setting(name)
        .unwrap_or_else(|_| default.to_string())
        .parse::<u64>()
        .map_err(|error| ConfigError::InvalidScheduler(format!("{name}: {error}")))?;
    if value == 0 {
        return Err(ConfigError::InvalidScheduler(format!(
            "{name} must be greater than zero"
        )));
    }
    Ok(value)
}

fn scheduler_bool(name: &'static str, default: bool) -> Result<bool, ConfigError> {
    setting(name)
        .unwrap_or_else(|_| default.to_string())
        .parse::<bool>()
        .map_err(|error| ConfigError::InvalidScheduler(format!("{name}: {error}")))
}

fn load_jev() -> Result<Option<crate::jev::JevConfig>, ConfigError> {
    match setting("JEV_ENABLED").as_deref() {
        Err(_) | Ok("false") => return Ok(None),
        Ok("true") => {}
        _ => return Err(ConfigError::InvalidJev),
    }
    let api_key =
        setting("TYPESAFE_API_KEY").map_err(|_| ConfigError::Missing("TYPESAFE_API_KEY"))?;
    let model = setting("JEV_MODEL").map_err(|_| ConfigError::Missing("JEV_MODEL"))?;
    if api_key.trim().is_empty()
        || model.trim().is_empty()
        || model.ends_with("latest")
        || model.ends_with("preview")
    {
        return Err(ConfigError::InvalidJev);
    }
    let descriptions = serde_json::from_str(
        &setting("JEV_MODEL_DESCRIPTIONS_JSON").unwrap_or_else(|_| "{}".into()),
    )
    .map_err(|_| ConfigError::InvalidJev)?;
    Ok(Some(crate::jev::JevConfig {
        api_key,
        model,
        descriptions,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_capture_defaults_on_and_requires_explicit_boolean() {
        assert!(parse_content_capture_enabled(None).unwrap());
        assert!(parse_content_capture_enabled(Some("true")).unwrap());
        assert!(!parse_content_capture_enabled(Some("false")).unwrap());
        assert!(parse_content_capture_enabled(Some("no")).is_err());
        assert!(parse_content_capture_enabled(Some("")).is_err());
    }

    #[test]
    fn content_capture_pool_budget_reserves_scheduler_and_maintenance_connections() {
        assert!(validate_capture_pool_budget(3, 1, true, false).is_err());
        assert!(validate_capture_pool_budget(4, 1, true, false).is_err());
        assert!(validate_capture_pool_budget(5, 1, true, false).is_ok());
        assert!(validate_capture_pool_budget(3, 1, false, false).is_ok());
        assert!(validate_capture_pool_budget(3, 1, true, true).is_err());
        assert!(validate_capture_pool_budget(4, 1, true, true).is_ok());
    }

    #[test]
    fn optional_provider_keys_in_production() {
        if let Ok(case) = env::var("GATEWAY_CONFIG_TEST_CASE") {
            let parsed = Config::from_env();
            if case == "placeholder" {
                assert!(matches!(
                    parsed,
                    Err(ConfigError::DevelopmentSecretOutsideDevelopment(
                        "OPENAI_API_KEY"
                    ))
                ));
            } else if case.starts_with("codex-") {
                if case == "codex-owner" {
                    assert_eq!(
                        parsed.unwrap().codex_owner_user_id,
                        Some(Uuid::from_u128(1))
                    );
                } else {
                    assert!(matches!(parsed, Err(ConfigError::InvalidCodexOwner)));
                }
            } else {
                let mut config = parsed.unwrap();
                assert_eq!(config.openai_api_key.is_empty(), case != "keys");
                assert_eq!(config.anthropic_api_key.is_empty(), case != "keys");
                assert_eq!(config.anthropic_client_oauth, case == "oauth");
                config.provider_allowed_tenant_ids.push(Uuid::nil());
                let runtime = tokio::runtime::Runtime::new().unwrap();
                runtime.block_on(async {
                    use crate::model_registry::{
                        CapabilityRequirements, GatewayOperation, ModelSelector, RegistryError,
                    };
                    let pool = sqlx::postgres::PgPoolOptions::new()
                        .connect_lazy(&config.database_url)
                        .unwrap();
                    let state = crate::state::AppState::new(config, pool).unwrap();
                    for operation in [
                        GatewayOperation::OpenAiResponses,
                        GatewayOperation::AnthropicMessages,
                    ] {
                        let result = state
                            .models
                            .candidates(
                                Uuid::nil(),
                                true,
                                operation,
                                ModelSelector::parse("company-default").unwrap(),
                                CapabilityRequirements::default(),
                            )
                            .await;
                        let expected = match (case.as_str(), operation) {
                            ("keys", GatewayOperation::OpenAiResponses) => {
                                Some("env://OPENAI_API_KEY")
                            }
                            ("keys", GatewayOperation::AnthropicMessages) => {
                                Some("env://ANTHROPIC_API_KEY")
                            }
                            ("oauth", GatewayOperation::AnthropicMessages) => {
                                Some("env://ANTHROPIC_CLIENT_OAUTH")
                            }
                            ("token", GatewayOperation::AnthropicMessages) => {
                                Some("env://ANTHROPIC_AUTH_TOKEN")
                            }
                            _ => None,
                        };
                        if let Some(expected) = expected {
                            assert_eq!(result.unwrap()[0].profile.credential_ref, expected);
                        } else {
                            assert!(matches!(result, Err(RegistryError::UntrustedProvider)));
                        }
                    }
                });
            }
            return;
        }
        for case in [
            "absent",
            "blank",
            "oauth",
            "token",
            "keys",
            "placeholder",
            "codex-missing",
            "codex-invalid",
            "codex-owner",
        ] {
            let spool =
                std::env::temp_dir().join(format!("gateway-config-{}-{case}", Uuid::new_v4()));
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .env_clear()
                .args([
                    "--exact",
                    "config::tests::optional_provider_keys_in_production",
                    "--nocapture",
                ])
                .env("GATEWAY_CONFIG_TEST_CASE", case)
                .env(
                    "DATABASE_URL",
                    "postgres://fixture:fixture@127.0.0.1:1/fixture",
                )
                .env("CREDENTIAL_ENVIRONMENT", "prod")
                .env(
                    "CREDENTIAL_HASH_KEY",
                    "fixture-config-validation-32-bytes-long",
                )
                .env("PROVIDER_ALLOWED_TENANT_IDS", Uuid::new_v4().to_string())
                .env("LEDGER_SPOOL_DIR", &spool);
            match case {
                "blank" => {
                    command
                        .env("OPENAI_API_KEY", " ")
                        .env("ANTHROPIC_API_KEY", " ");
                }
                "oauth" => {
                    command
                        .env("ANTHROPIC_CLIENT_OAUTH", "true")
                        .env("ANTHROPIC_OAUTH_CREDENTIAL_ID", Uuid::new_v4().to_string());
                }
                "token" => {
                    command.env("ANTHROPIC_AUTH_TOKEN", "fixture-auth-token");
                }
                "keys" => {
                    command
                        .env("OPENAI_API_KEY", "fixture-openai-key")
                        .env("ANTHROPIC_API_KEY", "fixture-anthropic-key");
                }
                "placeholder" => {
                    command.env("OPENAI_API_KEY", "replace-me");
                }
                "codex-missing" | "codex-invalid" | "codex-owner" => {
                    command.env("CODEX_AUTH_FILE", "/fixture/auth.json");
                    if case == "codex-invalid" {
                        command.env("CODEX_OWNER_USER_ID", "invalid");
                    } else if case == "codex-owner" {
                        command.env("CODEX_OWNER_USER_ID", Uuid::from_u128(1).to_string());
                    }
                }
                _ => {}
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{case}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if spool.exists() {
                std::fs::remove_dir_all(spool).unwrap();
            }
        }
    }

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
    fn an_empty_admin_bootstrap_tenant_means_no_bootstrap() {
        let tenant = Uuid::new_v4();
        assert_eq!(admin_bootstrap_tenant_id(None).unwrap(), None);
        // What a deployment template forwards when the operator set nothing.
        assert_eq!(
            admin_bootstrap_tenant_id(Some(String::new())).unwrap(),
            None
        );
        assert_eq!(
            admin_bootstrap_tenant_id(Some("  ".to_owned())).unwrap(),
            None
        );
        assert_eq!(
            admin_bootstrap_tenant_id(Some(format!(" {tenant} "))).unwrap(),
            Some(tenant)
        );
        assert!(matches!(
            admin_bootstrap_tenant_id(Some("not-a-uuid".to_owned())),
            Err(ConfigError::InvalidAdminBootstrapTenant(_))
        ));
        assert!(matches!(
            admin_bootstrap_tenant_id(Some(Uuid::nil().to_string())),
            Err(ConfigError::NilTenant)
        ));
    }

    #[test]
    fn an_explicitly_empty_model_allowlist_allows_nothing() {
        assert!(
            model_allowlist(Some(String::new())).is_empty(),
            "an empty list must not fall back to a default model"
        );
        assert!(model_allowlist(Some(" , ".to_owned())).is_empty());
        // Leaving the variable out is a different thing entirely.
        assert_eq!(model_allowlist(None), vec!["company-default".to_owned()]);
        assert_eq!(
            model_allowlist(Some(" a , b ".to_owned())),
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    #[test]
    fn an_empty_setting_reads_as_not_set() {
        // SAFETY: writing an environment variable races with any thread that
        // reads the environment. Nothing else in this binary reads this name,
        // and it is removed before the test ends.
        let name = "AGENT_POOL_GATEWAY_EMPTY_SETTING_PROBE";
        // What a deployment template forwards for a variable with no value.
        unsafe { env::set_var(name, "") };
        assert!(
            setting(name).is_err(),
            "an empty value must read as not set"
        );
        unsafe { env::set_var(name, "   ") };
        assert!(setting(name).is_err(), "a blank value must read as not set");
        // A real value survives untouched: a secret may carry surrounding
        // whitespace, so the value is never trimmed on the way out.
        unsafe { env::set_var(name, " kept ") };
        assert_eq!(setting(name).unwrap(), " kept ");
        unsafe { env::remove_var(name) };
    }

    #[test]
    fn repository_placeholder_secrets_stay_in_development() {
        // The exact strings the compose file and .env.example ship with.
        for placeholder in REPOSITORY_PLACEHOLDER_SECRETS {
            assert!(
                reject_placeholder_secret("dev", "CREDENTIAL_HASH_KEY", placeholder).is_ok(),
                "development must keep working"
            );
            assert!(matches!(
                reject_placeholder_secret("prod", "CREDENTIAL_HASH_KEY", placeholder),
                Err(ConfigError::DevelopmentSecretOutsideDevelopment(
                    "CREDENTIAL_HASH_KEY"
                ))
            ));
            assert!(matches!(
                reject_placeholder_secret(
                    "staging",
                    "OPENAI_API_KEY",
                    &format!("  {placeholder} ")
                ),
                Err(ConfigError::DevelopmentSecretOutsideDevelopment(
                    "OPENAI_API_KEY"
                ))
            ));
        }
        reject_placeholder_secret(
            "prod",
            "CREDENTIAL_HASH_KEY",
            "a-real-secret-at-least-32-bytes-long",
        )
        .unwrap();
    }

    #[test]
    fn providers_need_at_least_one_tenant() {
        let tenant = Uuid::new_v4();
        let bootstrap = Uuid::new_v4();
        // What the shipped compose file passes when the operator set nothing.
        assert!(matches!(
            provider_allowed_tenants(String::new(), None, false),
            Err(ConfigError::NoAllowedTenant)
        ));
        assert_eq!(
            provider_allowed_tenants(tenant.to_string(), None, false).unwrap(),
            vec![tenant]
        );
        assert_eq!(
            provider_allowed_tenants(String::new(), Some(bootstrap), false).unwrap(),
            vec![bootstrap]
        );
        assert_eq!(
            provider_allowed_tenants(String::new(), None, true).unwrap(),
            vec![Uuid::nil()]
        );
        // The bootstrap tenant is not added twice when it is already listed.
        assert_eq!(
            provider_allowed_tenants(tenant.to_string(), Some(tenant), false).unwrap(),
            vec![tenant]
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
            validate_pool_budget(3, 2),
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

    #[test]
    fn ollama_native_messages_requires_a_separate_profile() {
        let native = r#"[{"kind":"ollama","credential_ref":"none://ollama-messages","provider_key":"local-claude","base_url":"http://127.0.0.1:11434","operations":["anthropic_messages"]}]"#;
        assert!(extra_provider_profiles(native.to_owned(), false).is_ok());
        for operations in [
            r#"["anthropic_messages","openai_responses"]"#,
            r#"["anthropic_count_tokens"]"#,
        ] {
            let invalid = native.replace(r#"["anthropic_messages"]"#, operations);
            assert!(extra_provider_profiles(invalid, false).is_err());
        }
        assert!(extra_provider_profiles(native.replace("ollama", "vllm"), false).is_err());
    }

    #[test]
    fn extra_provider_profiles_are_typed_and_local_auth_is_optional() {
        let personal_anthropic = r#"[{"kind":"anthropic","credential_ref":"env://ANTHROPIC_PERSONAL_API","provider_key":"anthropic-api","base_url":"https://example.test","operations":["anthropic_messages"]}]"#;
        assert!(extra_provider_profiles(personal_anthropic.to_owned(), true).is_ok());
        assert!(extra_provider_profiles(personal_anthropic.to_owned(), false).is_err());
        let profiles = extra_provider_profiles(
            r#"[{"kind":"vllm","credential_ref":"none://vllm","provider_key":"local-vllm","base_url":"http://127.0.0.1:8000/","operations":["openai_responses"],"secret_env":null}]"#.to_owned(),
            false,
        )
        .unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].kind, ProviderKind::Vllm);
        assert_eq!(profiles[0].base_url, "http://127.0.0.1:8000");
        assert!(profiles[0].secret.is_none());
        assert!(extra_provider_profiles(
            r#"[{"kind":"unknown","credential_ref":"x","provider_key":"x","base_url":"http://localhost","operations":["openai_responses"]}]"#.to_owned(),
            false,
        )
        .is_err());
        assert!(extra_provider_profiles(
            r#"[{"kind":"gemini","credential_ref":"x","provider_key":"x","base_url":"https://example.test","operations":["openai_responses"]}]"#.to_owned(),
            false,
        )
        .is_err());
    }
}
