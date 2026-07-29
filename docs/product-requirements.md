# Product requirements

## Roles

- Platform owner: provider pools, global policy and emergency controls.
- Organization admin: teams, people, keys, budgets and reports.
- Team manager: team-scoped budgets and model permissions.
- Auditor: read-only audit and usage exports.
- User/service: own key metadata and usage, never provider credentials.

## Policy dimensions

Principal/team, model alias, provider class, RPM, TPM, concurrency, daily and
monthly spend, request token ceiling, time window, environment, IP/network,
data-retention class and priority.

## MVP acceptance

1. Admin can create, rotate and revoke a virtual key; plaintext is returned once.
2. A request is denied before upstream use when key, model or budget is invalid.
3. Concurrent requests cannot exceed a budget through race conditions.
4. Streaming and non-streaming OpenAI-compatible chat requests work.
5. Usage is reconciled and queryable by principal, team, model and provider.
6. Provider failure triggers bounded fallback without exposing secrets.
7. Every admin mutation and route decision has a correlation ID and audit event.
8. No request content is stored unless an explicit retention policy enables it.

## Later phases

OIDC/SCIM, approval workflows, cost allocation, chargeback exports, semantic
caching, prompt DLP, regional routing, additional protocol adapters and a Rust
admin web UI.
