# Security and compliance

## Supported credential sources

- Each user's own API keys and service accounts.
- Enterprise cloud model endpoints such as approved Azure/AWS/GCP deployments.
- Contractually approved provider gateways.
- The owner's own Claude Code subscription, held by the gateway container
  (decided 2026-10-29; terms in `terminology.md`). The container keeps the
  `sk-ant-oat` token in the file named by `ANTHROPIC_SUBSCRIPTION_TOKEN_FILE`
  (mode 0600), never in the database or logs and never returned to a client.
  An admin replaces it with `PUT /admin/v1/provider-tokens/anthropic`; each
  replacement is audited by fingerprint only. Clients send only a Gateway key,
  which identifies the person and is not a model credential.
- Before this change the running container used `ANTHROPIC_CLIENT_OAUTH`
  (the client's own token was forwarded). That mode still exists and cannot be
  combined with the held token.
- Not verified: whether Anthropic's terms allow a server to use a
  `claude setup-token` token on the owner's behalf. See `terminology.md`.

## Explicitly unsupported

- Logging in for other people, pooling or rotating subscription logins, or using
  one person's login for another person's requests.
- Capturing browser cookies, OAuth refresh tokens or desktop session tokens.
- Sharing named seats between employees or bypassing product usage limits.
- Concealing automation from a provider or evading throttling.

If a provider offers a documented enterprise capacity-sharing feature, add it as
a distinct integration only after legal/security review.

## Seat subscriptions and API billing

| Client mode | What pays for inference | Gateway policy |
| --- | --- | --- |
| Claude Code with a Gateway key only | The owner's subscription token held by the container (see `architecture.md`); the client's own claude.ai login is not used | Running in production; per-user token isolation is not implemented |
| Claude Code with only a base URL override | Developer's saved claude.ai login stays active; the subscription pays | Legacy mode while `ANTHROPIC_CLIENT_OAUTH` is on: the client's token is validated and forwarded, never stored; cannot be combined with the held token |
| Codex signed in with ChatGPT | ChatGPT plan allowance and billing | Outside this gateway credential path |
| Codex with an OpenAI API key against OpenAI | OpenAI API pricing, billed separately from ChatGPT | Outside the gateway credential path |
| Codex custom provider with `env_key`/`auth` and `requires_openai_auth = false` | Gateway/provider contract; client-side ChatGPT plan-meter interaction is undocumented | Supported; no ChatGPT login token is forwarded |
| Codex custom provider with `requires_openai_auth = true` and ChatGPT login | ChatGPT/OpenAI authentication path | Unsupported: never relay personal or named-seat login tokens |
| Codex custom provider with `requires_openai_auth = true` and OpenAI API key | OpenAI API pricing | Unsupported on the gateway path: use a dedicated gateway credential instead |
| Codex custom provider without authentication | No attributable credential owner | Unsupported in production |
| VS Code Custom Endpoint | BYOK/provider account; no Copilot plan is required for that model | Supported for chat/agent APIs |
| Cursor BYOK/custom endpoint | Provider usage is at the user's own cost; Cursor seat-quota interaction is undocumented | Partial; never claim the seat becomes provider credit |

OpenAI documents that ChatGPT and API billing are separate, and that Codex with
ChatGPT sign-in uses the ChatGPT plan while an OpenAI API key uses API pricing.
Those sources do not document custom-provider plan metering; that interaction is
therefore `unknown`. Anthropic documents the equivalent gateway-credential boundary
for Claude Code. These facts do **not** authorize pooling named seats, forwarding
login tokens, or converting a subscription allowance into reusable API credit.

Sources, verified 2026-09-11:

- <https://code.claude.com/docs/en/llm-gateway>
- <https://code.claude.com/docs/en/env-vars>
- <https://help.openai.com/en/articles/20001275>
- <https://help.openai.com/en/articles/9039756>
- <https://learn.chatgpt.com/docs/auth>
- <https://learn.chatgpt.com/docs/config-file/config-reference>
- <https://code.visualstudio.com/docs/agent-customization/language-models>
- <https://prod.cursor.com/help/models-and-usage/api-keys>

## Controls

- TLS everywhere; provider adapters run in-process, upstream provider endpoints
  are reached only from the gateway process.
- Virtual keys prefixed by environment and tenant, stored as keyed hashes.
- Provider credentials in a managed secret store with rotation and access logs.
- Least-privilege RBAC; SSO MFA for admins; break-glass accounts monitored.
- Request/response bodies never enter logs, traces or metrics. Content goes
  only to the dedicated encrypted store (trial default on, 2026-10-29; see
  `request-response-storage.md`), with expiry and deletion.
- Secrets and common personal data redacted before telemetry export.
- Immutable audit events for key creation/revocation, policy edits, routing and
  administrative access.
- Per-tenant and per-principal isolation enforced in every query.

## Threat model highlights

Key theft, prompt/data exfiltration, malicious model selection, quota races,
provider credential leakage, admin abuse, replay, retry double-spend and
dependency compromise are release-blocking threat-model areas.

## Retention

Usage metadata and audit logs have separate retention periods. Content lives only in
the encrypted store with a per-tenant expiry (default 30 days). Deletion and legal-hold workflows must be tested before
production.
