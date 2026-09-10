use reqwest::Client;
use sha2::{Digest, Sha256};
use sqlx::{Pool, Postgres};
use std::time::Duration;

use crate::{
    config::Config,
    identity::CredentialService,
    ledger::{Ledger, LedgerError},
    metrics::Instruments,
};

/// Shared, read-only state every handler gets through `State`.
pub struct AppState {
    pub(crate) config: Config,
    pub(crate) client: Client,
    pub(crate) bootstrap_key_hash: Option<[u8; 32]>,
    /// Built here, which is after `metrics::init()` in `startup` -- an
    /// instrument binds to the provider installed at build time, so this
    /// ordering has to be visible rather than implied.
    pub(crate) instruments: Instruments,
    /// Lazy: built without touching the network, so handler code can assume a
    /// pool exists and readiness decides whether it currently works.
    pub(crate) pool: Pool<Postgres>,
    pub(crate) credentials: CredentialService,
    pub(crate) ledger: Ledger,
}

impl AppState {
    pub fn new(config: Config, pool: Pool<Postgres>) -> Result<Self, LedgerError> {
        let bootstrap_key_hash = (config.credential_environment == "dev")
            .then(|| {
                config
                    .bootstrap_client_key
                    .as_ref()
                    .map(|key| Sha256::digest(key.as_bytes()).into())
            })
            .flatten();
        let credentials = CredentialService::new(
            pool.clone(),
            config.credential_hash_key_id.clone(),
            config.credential_hash_key.clone(),
            config.credential_retiring_hash_keys.clone(),
            config.credential_environment.clone(),
            config.credential_cache_capacity,
            Duration::from_secs(config.credential_cache_ttl_seconds),
        );
        let ledger = Ledger::open(
            pool.clone(),
            config.ledger_spool_dir.clone(),
            config.ledger_spool_max_bytes,
            config.ledger_spool_entry_max_bytes,
        )?;
        Ok(Self {
            config,
            client: Client::new(),
            bootstrap_key_hash,
            instruments: Instruments::new(),
            pool,
            credentials,
            ledger,
        })
    }
}
