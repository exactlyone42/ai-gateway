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
Model registry administration and lifecycle ordering are documented in
`docs/model-registry.md`.
Request policy resolution, hints, constraints, and rollout are documented in
`docs/request-policy.md`.

## Current milestone

The repository contains the native Anthropic and OpenAI data-plane slice:

- `GET /healthz`
- `POST /v1/messages` and `/v1/messages/count_tokens`
- `POST /v1/responses` and `/v1/chat/completions`, including incremental SSE
- tenant-scoped `GET /v1/models` and deterministic routing from the active PostgreSQL registry
- tenant-scoped virtual credentials with constant-time HMAC verification
- seven-layer request policy resolution with immutable decision snapshots
- tenant-scoped hard/soft quota enforcement with persistent UTC windows and usage reconciliation
- cancellation propagation, durable usage accounting, request IDs, traces,
  metrics, and structured JSON logs

An explicitly configured bootstrap key and `ALLOWED_MODELS` remain a dev-only compatibility path. Issued virtual
credentials resolve through PostgreSQL to a user, device, client and agent.

The work from here is split into 18 independently reviewable segments. Read
`docs/work-breakdown.md` for the boundaries and dependency order, and
`loops/README.md` for what is in progress right now. The runbook sections below
(migration, first admin, create identities, issue keys, configure providers)
land with segments W02, W08 and W09; they are deliberately absent until the
commands actually exist.

## Run locally

```bash
cp .env.example .env
docker compose -f deploy/docker-compose.yml up -d --wait postgres
cargo run
curl http://127.0.0.1:8080/health   # liveness: the process answers
curl http://127.0.0.1:8080/ready    # readiness: the database answers too
```

Migrations run during startup, so a database is required to boot. A failing
migration stops the process rather than serving against a half-migrated schema.

The whole stack, including the gateway image. It runs without the development
bootstrap, so it needs one tenant that may use the server-side provider
credentials:

```bash
PROVIDER_ALLOWED_TENANT_IDS=<tenant uuid> \
  docker compose -f deploy/docker-compose.yml up --build
```

Without it the gateway refuses to start and says which setting is missing,
rather than serving providers no tenant may use.

The Admin Web UI is served by the gateway at `http://127.0.0.1:8080/admin/` and uses the
short-lived Admin session flow documented in `docs/admin.md`.

Never commit real provider credentials or client keys.

## Documentation

Start with `CLAUDE.md` — it holds the document index and says which file to read
for which problem, so you do not have to load everything.

- `docs/work-breakdown.md` — the 18 segments, dependency order, priorities
- `docs/engineering-standards.md` — quality gates and rules, machine-checkable
- `docs/task-ledger.md` — time and token cost per segment, plus trade-offs
- `docs/architecture.md` — three planes, request path, failure model
- `docs/security-and-compliance.md` — credential and subscription boundary
- `docs/admin.md` — Admin authentication, RBAC and control-plane API contract
- `docs/admin-web.md` — Admin report definitions, operator questions and browser security
- `docs/product-requirements.md` — roles, policies and acceptance criteria
- `docs/client-compatibility.md` — integration contract by client
- `docs/request-policy.md` — policy scopes, request hints, constraints and rollout
- `docs/operations.md` — rollout and incident controls
- `loops/` — one live status file per segment

## Quality gates

```bash
./scripts/check.sh gate   # while iterating: fmt-check, clippy, loop-file discipline
./scripts/check.sh all    # before a PR: everything CI runs
```

## License

Proprietary. All rights reserved.
