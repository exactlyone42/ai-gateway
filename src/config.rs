use std::{env, net::SocketAddr};

use thiserror::Error;

#[derive(Clone, Debug)]
pub struct Config {
    pub listen_address: SocketAddr,
    pub litellm_base_url: String,
    pub litellm_master_key: String,
    pub bootstrap_client_key: String,
    pub allowed_models: Vec<String>,
    /// `OTEL_EXPORTER_OTLP_ENDPOINT`. None means no exporter and no tracer
    /// provider at all, so log records carry no `trace_id` either -- only the
    /// request id. Correlating one request across services needs this set.
    pub otlp_endpoint: Option<String>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
    #[error("invalid LISTEN_ADDRESS: {0}")]
    InvalidAddress(#[from] std::net::AddrParseError),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let listen_address = env::var("LISTEN_ADDRESS")
            .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
            .parse()?;
        let litellm_base_url = required("LITELLM_BASE_URL")?
            .trim_end_matches('/')
            .to_owned();
        let litellm_master_key = required("LITELLM_MASTER_KEY")?;
        let bootstrap_client_key = required("BOOTSTRAP_CLIENT_KEY")?;
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

        Ok(Self {
            listen_address,
            litellm_base_url,
            litellm_master_key,
            bootstrap_client_key,
            allowed_models,
            otlp_endpoint,
        })
    }
}

fn required(name: &'static str) -> Result<String, ConfigError> {
    env::var(name).map_err(|_| ConfigError::Missing(name))
}
