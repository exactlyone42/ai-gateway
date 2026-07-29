# Architecture

## Goals

Issue a revocable virtual API key to each employee or service, maximize approved
API capacity utilization, enforce per-user model and budget policy, and preserve
a complete audit trail without exposing provider credentials.

## Request path

```text
IDE / agent
  -> Rust gateway (TLS, virtual key, identity, policy, quota)
  -> LiteLLM proxy (model alias, provider API key routing, fallback)
  -> approved provider API / enterprise endpoint
  -> usage reconciliation and append-only audit events
```

The Rust gateway is the policy authority. LiteLLM is an internal-only upstream
and must not be reachable by end users.

## Components

1. **Rust data plane**: OpenAI-compatible endpoints, streaming proxy, key
   validation, model authorization, rate limits, budget reservations and usage
   recording.
2. **Rust control plane**: people, teams, service accounts, key lifecycle,
   policies, provider pools, model catalog, budget periods and audit queries.
3. **LiteLLM**: normalized provider calls, model aliases and approved provider
   failover. Provider secrets live in a secret manager, never in client config.
4. **PostgreSQL**: identities, policies, hashed keys, budgets, usage ledger and
   configuration versions.
5. **Redis-compatible store**: distributed RPM/TPM counters, concurrency leases,
   circuit-breaker state and short-lived budget reservations.
6. **OIDC/SSO**: admin authentication and group synchronization.
7. **Telemetry**: OpenTelemetry metrics/traces, redacted structured logs and
   alerts for spend, error rate, latency and pool exhaustion.

## Routing

Policies resolve in this order: global deny -> user/team deny -> model allowlist
-> time/budget limits -> provider health -> weighted least-utilized eligible
deployment. Fallback occurs only between approved API deployments of the same
policy class. Retries use idempotency keys where supported and never retry an
ambiguous non-idempotent response blindly.

## Quotas and fairness

- RPM, TPM, concurrent requests and daily/monthly currency budgets.
- Atomic pre-request token estimate reservation; reconcile against provider
  usage after response.
- Hierarchical limits: company -> department -> team -> principal -> key.
- Weighted fair queueing prevents one heavy user from starving others.
- Emergency reserve protects production automation from interactive workloads.

## Data model

Core entities: `Principal`, `Team`, `VirtualKey`, `Policy`, `ModelAlias`,
`ProviderDeployment`, `Budget`, `UsageEvent`, `AuditEvent`, `RouteDecision`,
`SecretReference` and `ConfigVersion`. Raw keys are shown once; only keyed hashes
are stored. Provider secrets are represented by secret-manager references.

## Deployment

Run at least two stateless gateway replicas behind a private load balancer.
Place LiteLLM, PostgreSQL and Redis on private networks. Use mTLS or workload
identity between gateway and LiteLLM. Production changes use versioned config,
four-eyes approval for provider credentials and canary rollout with rollback.
