# Agent Pool Gateway

Private, company-grade AI access gateway. It issues one revocable key per
person/device/client/agent, resolves that key to a human-readable identity,
applies policy, schedules the request, and routes it to an approved provider API
account. Provider credentials never leave the server.

The gateway implements the provider adapters itself. LiteLLM is **not** a runtime
dependency.

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
- `POST /v1/chat/completions` (non-streaming, forwards to a configured upstream)
- bearer-key authentication with constant-time comparison
- model allowlist from configuration
- request IDs and structured JSON logs

The bootstrap key is for local development only.

The work from here is split into 18 independently reviewable segments. Read
`docs/work-breakdown.md` for the boundaries and dependency order, and
`loops/README.md` for what is in progress right now. The runbook sections below
(migration, first admin, create identities, issue keys, configure providers)
land with segments W02, W03 and W09; they are deliberately absent until the
commands actually exist.

## Run locally

```bash
cp .env.example .env
cargo run
curl http://127.0.0.1:8080/healthz
```

Never commit real provider credentials or client keys.

## Documentation

Start with `CLAUDE.md` — it holds the document index and says which file to read
for which problem, so you do not have to load everything.

- `docs/work-breakdown.md` — the 18 segments, dependency order, priorities
- `docs/engineering-standards.md` — quality gates and rules, machine-checkable
- `docs/task-ledger.md` — time and token cost per segment, plus trade-offs
- `docs/architecture.md` — three planes, request path, failure model
- `docs/security-and-compliance.md` — credential and subscription boundary
- `docs/product-requirements.md` — roles, policies and acceptance criteria
- `docs/client-compatibility.md` — integration contract by client
- `docs/operations.md` — rollout and incident controls
- `loops/` — one live status file per segment

## Quality gates

```bash
./scripts/check.sh gate   # while iterating: fmt-check, clippy, loop-file discipline
./scripts/check.sh all    # before a PR: everything CI runs
```

## License

Proprietary. All rights reserved.
