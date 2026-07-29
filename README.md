# Agent Pool Gateway

Private, company-grade AI access gateway for issuing one virtual key per person,
enforcing quotas and model policies, and routing requests through LiteLLM to
approved provider API accounts.

## Important boundary

This project pools **official API capacity**, not consumer or business
subscription logins. It does not extract session tokens, share seats, automate
interactive logins, or rotate Claude Code/Codex subscription accounts. Those
patterns can violate provider terms and weaken auditability. Subscription seats
remain assigned to their users; shared usage goes through organization API
accounts or approved enterprise endpoints.

## Target clients

Any tool that supports an OpenAI-compatible base URL and API key can connect
directly. Others use a small, explicit adapter:

- Hermes and VS Code extensions: OpenAI-compatible endpoint.
- Claude Code / Claude Desktop: supported only where the product exposes an
  approved custom API/provider configuration; no login interception.
- Codex CLI/Desktop: supported when custom provider/base URL configuration is
  available under the organization's product policy.
- Orca and other IDEs: OpenAI-compatible endpoint or a documented adapter.

Exact compatibility is tracked in `docs/client-compatibility.md`; product
capabilities can change and must be verified before declaring support.

## Current milestone

The repository contains the first compilable Rust data-plane slice:

- `GET /healthz`
- `POST /v1/chat/completions`
- bearer-key authentication
- model allowlist
- forwarding to a LiteLLM upstream
- request IDs and structured logs

The bootstrap key is for local development only. The production key store,
quota engine, admin API, audit ledger, SSO/RBAC, streaming and provider health
routing are explicitly tracked on the project board.

## Run locally

```bash
cp .env.example .env
cargo run
curl http://127.0.0.1:8080/healthz
```

Never commit real provider credentials or client keys.

## Documentation

- `docs/architecture.md` — components, data flow and deployment
- `docs/security-and-compliance.md` — credential and subscription boundary
- `docs/product-requirements.md` — roles, policies and acceptance criteria
- `docs/client-compatibility.md` — integration contract by client
- `docs/operations.md` — rollout and incident controls

## License

Proprietary. All rights reserved.
