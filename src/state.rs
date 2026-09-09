use reqwest::Client;
use sha2::{Digest, Sha256};
use sqlx::{Pool, Postgres};

use crate::{config::Config, metrics::Instruments};

/// Shared, read-only state every handler gets through `State`.
pub struct AppState {
    pub(crate) config: Config,
    pub(crate) client: Client,
    pub(crate) bootstrap_key_hash: [u8; 32],
    /// Built here, which is after `metrics::init()` in `startup` -- an
    /// instrument binds to the provider installed at build time, so this
    /// ordering has to be visible rather than implied.
    pub(crate) instruments: Instruments,
    /// Lazy: built without touching the network, so handler code can assume a
    /// pool exists and readiness decides whether it currently works.
    pub(crate) pool: Pool<Postgres>,
}

impl AppState {
    pub fn new(config: Config, pool: Pool<Postgres>) -> Self {
        let bootstrap_key_hash = Sha256::digest(config.bootstrap_client_key.as_bytes()).into();
        Self {
            config,
            client: Client::new(),
            bootstrap_key_hash,
            instruments: Instruments::new(),
            pool,
        }
    }
}
