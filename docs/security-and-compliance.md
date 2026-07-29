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

## Controls

- TLS everywhere; internal LiteLLM endpoint not publicly routable.
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
