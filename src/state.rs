use reqwest::Client;
use sha2::{Digest, Sha256};

use crate::config::Config;

/// Shared, read-only state every handler gets through `State`.
pub struct AppState {
    pub(crate) config: Config,
    pub(crate) client: Client,
    pub(crate) bootstrap_key_hash: [u8; 32],
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let bootstrap_key_hash = Sha256::digest(config.bootstrap_client_key.as_bytes()).into();
        Self {
            config,
            client: Client::new(),
            bootstrap_key_hash,
        }
    }
}
