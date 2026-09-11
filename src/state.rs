use reqwest::{Client, redirect::Policy};
use sha2::{Digest, Sha256};
use sqlx::{Pool, Postgres, postgres::PgPoolOptions};
use std::{sync::Arc, time::Duration};

use crate::{
    admin::{AdminService, AdminServiceConfig},
    config::Config,
    identity::CredentialService,
    ledger::{Ledger, LedgerError},
    metrics::Instruments,
    model_registry::{
        GatewayOperation, ModelRegistry, ProviderDialect, ProviderProfile, ProviderProfiles,
    },
    policy::PolicyEngine,
    quota::QuotaEngine,
    scheduler::Scheduler,
};
use thiserror::Error;

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
    pub(crate) models: ModelRegistry,
    pub(crate) policies: PolicyEngine,
    pub(crate) quotas: QuotaEngine,
    pub(crate) scheduler: Scheduler,
    pub(crate) credentials: CredentialService,
    pub(crate) admin: AdminService,
    pub(crate) ledger: Ledger,
}

impl AppState {
    pub fn new(config: Config, pool: Pool<Postgres>) -> Result<Self, StateError> {
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
        let admin_pool = PgPoolOptions::new()
            .max_connections(config.admin_database_max_connections)
            .acquire_timeout(Duration::from_secs(2))
            .connect_lazy_with(pool.connect_options().as_ref().clone());
        let profiles = ProviderProfiles::new([
            ProviderProfile::bearer(
                "env://OPENAI_API_KEY",
                config.openai_provider.clone(),
                config.openai_base_url.clone(),
                ProviderDialect::OpenAi,
                vec![
                    GatewayOperation::OpenAiResponses,
                    GatewayOperation::OpenAiChatCompletions,
                ],
                config.provider_allowed_tenant_ids.clone(),
                Arc::<str>::from(config.openai_api_key.clone()),
            ),
            ProviderProfile::api_key(
                "env://ANTHROPIC_API_KEY",
                config.anthropic_provider.clone(),
                config.anthropic_base_url.clone(),
                ProviderDialect::Anthropic,
                vec![
                    GatewayOperation::AnthropicMessages,
                    GatewayOperation::AnthropicCountTokens,
                ],
                config.provider_allowed_tenant_ids.clone(),
                Arc::<str>::from(config.anthropic_api_key.clone()),
            ),
        ])?;
        let policies = PolicyEngine::with_cache(
            pool.clone(),
            config.policy_cache_capacity,
            Duration::from_secs(config.policy_cache_ttl_seconds),
        );
        let quotas = QuotaEngine::new(pool.clone());
        let scheduler = Scheduler::new(config.scheduler.clone())?;
        let admin = AdminService::new(
            admin_pool,
            credentials.clone(),
            AdminServiceConfig {
                hash_key: config.credential_hash_key.clone(),
                hash_key_id: config.credential_hash_key_id.clone(),
                retiring_hash_keys: config.credential_retiring_hash_keys.clone(),
                bootstrap_token: config.admin_bootstrap_token.clone(),
                bootstrap_tenant_id: config.admin_bootstrap_tenant_id,
                session_ttl: Duration::from_secs(config.admin_session_ttl_seconds),
            },
        )
        .with_provider_profiles(profiles.clone())
        .with_policy_engine(policies.clone())
        .with_scheduler(scheduler.clone());
        let ledger = Ledger::open(
            pool.clone(),
            config.ledger_spool_dir.clone(),
            config.ledger_spool_max_bytes,
            config.ledger_spool_entry_max_bytes,
        )?;
        let models = ModelRegistry::new(pool.clone(), profiles, config.allowed_models.clone());
        let client = Client::builder().redirect(Policy::none()).build()?;
        Ok(Self {
            config,
            client,
            bootstrap_key_hash,
            instruments: Instruments::new(),
            pool,
            models,
            policies,
            quotas,
            scheduler,
            credentials,
            admin,
            ledger,
        })
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("could not initialize the usage ledger")]
    Ledger(#[from] LedgerError),
    #[error("could not initialize the provider client")]
    Http(#[from] reqwest::Error),
    #[error("could not initialize the model registry")]
    Registry(#[from] crate::model_registry::RegistryError),
    #[error("could not initialize the scheduler")]
    Scheduler(#[from] crate::scheduler::SchedulerError),
}
