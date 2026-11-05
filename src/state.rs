use reqwest::{Client, redirect::Policy};
use sha2::{Digest, Sha256};
use sqlx::{Pool, Postgres, postgres::PgPoolOptions};
use std::{sync::Arc, time::Duration};

use crate::{
    admin::{AdminService, AdminServiceConfig},
    config::Config,
    identity::CredentialService,
    ledger::{Ledger, LedgerError},
    lifecycle::ProcessLifecycle,
    metrics::Instruments,
    model_registry::{
        GatewayOperation, ModelRegistry, ProviderDialect, ProviderKind, ProviderProfile,
        ProviderProfiles, RegistryError,
    },
    policy::PolicyEngine,
    quota::QuotaEngine,
    routing::RoutingEngine,
    scheduler::Scheduler,
};
use thiserror::Error;

/// Shared, read-only state every handler gets through `State`.
pub struct AppState {
    pub(crate) config: Config,
    pub(crate) client: Client,
    pub(crate) local_provider_client: Client,
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
    pub(crate) routing: RoutingEngine,
    pub(crate) jev: Option<crate::jev::JevRouter>,
    pub(crate) scheduler: Scheduler,
    pub(crate) credentials: CredentialService,
    pub(crate) admin: AdminService,
    pub(crate) ledger: Ledger,
    pub(crate) lifecycle: ProcessLifecycle,
    /// Bounds concurrent shadow decompositions; a busy gateway skips rather than queues.
    pub(crate) shadow_slots: std::sync::Arc<tokio::sync::Semaphore>,
    pub(crate) capture_slots: std::sync::Arc<tokio::sync::Semaphore>,
    pub(crate) capture_erasure: Option<Arc<crate::capture_erasure::ActiveErasure>>,
    #[cfg(test)]
    pub(crate) capture_test_enabled: bool,
    pub(crate) held_token: Option<crate::held_token::HeldToken>,
    pub(crate) personal_tokens: Option<crate::held_token::PersonalTokens>,
}

impl AppState {
    pub(crate) fn client_for_provider(&self, kind: ProviderKind) -> &Client {
        match kind {
            ProviderKind::Ollama | ProviderKind::Vllm => &self.local_provider_client,
            _ => &self.client,
        }
    }

    pub(crate) async fn profile_for_user(
        &self,
        principal: &crate::canonical::PrincipalRef,
        profile: &ProviderProfile,
    ) -> Result<ProviderProfile, crate::error::GatewayError> {
        let Some(store) = self.personal_tokens.as_ref() else {
            return Ok(profile.clone());
        };
        if profile.is_unauthenticated() {
            return Ok(profile.clone());
        }
        let user = principal
            .user_id
            .ok_or(crate::error::GatewayError::ProviderUnavailable)?;
        let is_subscription = profile.is_held_bearer();
        let api_store = store.api_store(&profile.provider_key);
        let selected = if is_subscription { store } else { &api_store };
        let (token, lock) = selected
            .get_with_lock(principal.tenant_id, user)
            .await
            .map_err(|error| {
                tracing::error!(error = %error, "personal provider token read failed");
                crate::error::GatewayError::ProviderUnavailable
            })?
            .ok_or(crate::error::GatewayError::ProviderUnavailable)?;
        if is_subscription {
            Ok(profile.with_personal_token_lock(token, lock))
        } else {
            profile
                .with_personal_api_key(token, lock)
                .ok_or(crate::error::GatewayError::ProviderUnavailable)
        }
    }

    pub(crate) fn shadow_context(&self) -> crate::services::task_decomposition::ShadowContext {
        crate::services::task_decomposition::ShadowContext {
            pool: self.pool.clone(),
            models: self.models.clone(),
            client: self.client.clone(),
            slots: self.shadow_slots.clone(),
        }
    }

    pub fn new(config: Config, pool: Pool<Postgres>) -> Result<Self, StateError> {
        let bootstrap_key_hash = (config.credential_environment == "dev")
            .then(|| {
                config
                    .bootstrap_client_key
                    .as_ref()
                    .map(|key| Sha256::digest(key.as_bytes()).into())
            })
            .flatten();
        let personal_tokens = config
            .anthropic_personal_token_dir
            .clone()
            .map(crate::held_token::PersonalTokens::new);
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
        let mut provider_profiles = Vec::new();
        if let Some(path) = &config.codex_auth_file {
            provider_profiles.push(ProviderProfile::codex_file(
                path.clone(),
                config.codex_account_id.clone(),
                config.provider_allowed_tenant_ids.clone(),
            ));
        }
        if config.openai_api_key.is_empty() && personal_tokens.is_some() {
            provider_profiles.push(ProviderProfile::personal_api_slot(
                ProviderKind::OpenAi,
                "env://OPENAI_API_KEY",
                config.openai_provider.clone(),
                config.openai_base_url.clone(),
                vec![
                    GatewayOperation::OpenAiResponses,
                    GatewayOperation::OpenAiChatCompletions,
                ],
                config.provider_allowed_tenant_ids.clone(),
            ));
        } else if !config.openai_api_key.is_empty() {
            provider_profiles.push(ProviderProfile::bearer(
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
            ));
        }
        let held_token = config
            .anthropic_subscription_token_file
            .clone()
            .map(|path| {
                crate::held_token::HeldToken::load(
                    path,
                    config.anthropic_subscription_token_seed.as_deref(),
                )
            })
            .transpose()?;
        if held_token.is_none()
            && personal_tokens.is_some()
            && config.anthropic_api_key.is_empty()
            && config.anthropic_auth_token.is_none()
        {
            provider_profiles.push(ProviderProfile::personal_api_slot(
                ProviderKind::Anthropic,
                "env://ANTHROPIC_API_KEY",
                config.anthropic_provider.clone(),
                config.anthropic_base_url.clone(),
                vec![
                    GatewayOperation::AnthropicMessages,
                    GatewayOperation::AnthropicCountTokens,
                ],
                config.provider_allowed_tenant_ids.clone(),
            ));
        } else if held_token.is_some()
            || config.anthropic_client_oauth
            || config.anthropic_auth_token.is_some()
            || !config.anthropic_api_key.is_empty()
        {
            provider_profiles.push(if let Some(ref token) = held_token {
                ProviderProfile::held_bearer(
                    "env://ANTHROPIC_SUBSCRIPTION_TOKEN_FILE",
                    config.anthropic_provider.clone(),
                    config.anthropic_base_url.clone(),
                    vec![
                        GatewayOperation::AnthropicMessages,
                        GatewayOperation::AnthropicCountTokens,
                    ],
                    config.provider_allowed_tenant_ids.clone(),
                    token.clone(),
                )
            } else if config.anthropic_client_oauth {
                ProviderProfile::client_bearer(
                    "env://ANTHROPIC_CLIENT_OAUTH",
                    config.anthropic_provider.clone(),
                    config.anthropic_base_url.clone(),
                    vec![
                        GatewayOperation::AnthropicMessages,
                        GatewayOperation::AnthropicCountTokens,
                    ],
                    config.provider_allowed_tenant_ids.clone(),
                )
            } else if let Some(ref token) = config.anthropic_auth_token {
                ProviderProfile::bearer(
                    "env://ANTHROPIC_AUTH_TOKEN",
                    config.anthropic_provider.clone(),
                    config.anthropic_base_url.clone(),
                    ProviderDialect::Anthropic,
                    vec![
                        GatewayOperation::AnthropicMessages,
                        GatewayOperation::AnthropicCountTokens,
                    ],
                    config.provider_allowed_tenant_ids.clone(),
                    Arc::<str>::from(token.clone()),
                )
            } else {
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
                )
            });
        }
        let extra_profiles = config
            .extra_provider_profiles
            .iter()
            .map(|extra| {
                let args = (
                    extra.credential_ref.clone(),
                    extra.provider_key.clone(),
                    extra.base_url.clone(),
                    extra.operations.clone(),
                    config.provider_allowed_tenant_ids.clone(),
                );
                Ok(match (extra.kind, extra.secret.as_ref()) {
                    (
                        ProviderKind::Anthropic
                        | ProviderKind::OpenAi
                        | ProviderKind::Gemini
                        | ProviderKind::Xai,
                        None,
                    ) if personal_tokens.is_some() => ProviderProfile::personal_api_slot(
                        extra.kind, args.0, args.1, args.2, args.3, args.4,
                    ),
                    (ProviderKind::Gemini, Some(secret)) => ProviderProfile::api_key_for(
                        extra.kind,
                        args.0,
                        args.1,
                        args.2,
                        args.3,
                        args.4,
                        Arc::<str>::from(secret.clone()),
                    ),
                    (
                        ProviderKind::Xai | ProviderKind::Vllm | ProviderKind::Ollama,
                        Some(secret),
                    ) => ProviderProfile::bearer_for(
                        extra.kind,
                        args.0,
                        args.1,
                        args.2,
                        args.3,
                        args.4,
                        Arc::<str>::from(secret.clone()),
                    ),
                    (ProviderKind::Vllm | ProviderKind::Ollama, None) => {
                        ProviderProfile::unauthenticated_for(
                            extra.kind, args.0, args.1, args.2, args.3, args.4,
                        )
                    }
                    _ => return Err(RegistryError::InvalidProviderProfile),
                })
            })
            .collect::<Result<Vec<_>, RegistryError>>()?;
        provider_profiles.extend(extra_profiles);
        let routing = RoutingEngine::new(
            config.routing,
            provider_profiles
                .iter()
                .map(|profile| profile.provider_key.clone()),
        );
        let profiles = ProviderProfiles::new(provider_profiles)?;
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
        let ledger = Ledger::open_coordinated(
            pool.clone(),
            config.ledger_spool_dir.clone(),
            config.ledger_spool_max_bytes,
            config.ledger_spool_entry_max_bytes,
            config
                .content_capture_enabled
                .then(|| config.ledger_spool_dir.clone()),
        )?;
        let models = ModelRegistry::new(pool.clone(), profiles, config.allowed_models.clone());
        let client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(config.provider_connect_timeout)
            .read_timeout(config.provider_idle_timeout)
            .build()?;
        let local_provider_client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(config.provider_connect_timeout)
            .read_timeout(config.local_provider_idle_timeout)
            .build()?;
        Ok(Self {
            jev: config.jev.clone().map(crate::jev::JevRouter::new),
            config,
            client,
            local_provider_client,
            bootstrap_key_hash,
            instruments: Instruments::new(),
            pool,
            models,
            policies,
            quotas,
            routing,
            scheduler,
            credentials,
            admin,
            ledger,
            lifecycle: ProcessLifecycle::default(),
            shadow_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(8)),
            capture_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(16)),
            capture_erasure: None,
            #[cfg(test)]
            capture_test_enabled: false,
            held_token,
            personal_tokens,
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
    #[error("could not load the held subscription token")]
    HeldToken(#[from] crate::held_token::HeldTokenError),
}
