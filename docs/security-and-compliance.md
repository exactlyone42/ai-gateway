# Security and compliance

## Supported credential sources

- Provider organization API keys and service accounts.
- Enterprise cloud model endpoints such as approved Azure/AWS/GCP deployments.
- Contractually approved provider gateways.

## Explicitly unsupported

- Pooling or rotating individual Claude Code, ChatGPT/Codex, or similar
  subscription logins.
- Capturing browser cookies, OAuth refresh tokens or desktop session tokens.
- Sharing named seats between employees or bypassing product usage limits.
- Concealing automation from a provider or evading throttling.

If a provider offers a documented enterprise capacity-sharing feature, add it as
a distinct integration only after legal/security review.

## Seat subscriptions and API billing

| Client mode | What pays for inference | Gateway policy |
| --- | --- | --- |
| Claude Code with gateway credential | Provider account behind that credential; claude.ai subscription is not used | Supported |
| Claude Code with only a base URL override | Developer's saved claude.ai login can remain active | Unsupported: do not relay personal OAuth subscription credentials |
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
- Request/response bodies disabled from logs by default. Optional content
  retention requires a policy, encryption key, purpose and expiry.
- Secrets and common personal data redacted before telemetry export.
- Immutable audit events for key creation/revocation, policy edits, routing and
  administrative access.
- Per-tenant and per-principal isolation enforced in every query.

## Threat model highlights

Key theft, prompt/data exfiltration, malicious model selection, quota races,
provider credential leakage, admin abuse, replay, retry double-spend and
dependency compromise are release-blocking threat-model areas.

## Retention

Usage metadata and audit logs have separate retention periods. Content is not
retained by default. Deletion and legal-hold workflows must be tested before
production.
