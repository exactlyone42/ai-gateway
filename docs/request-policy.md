# Request policy

The gateway evaluates one immutable policy snapshot before model selection and
before any provider request. The snapshot is stored in
`gateway_requests.policy_decision`; later policy edits cannot change an
in-flight or historical decision.

## Resolution order

Active policies are tenant scoped and resolve from broadest to narrowest:

1. Global (the tenant/company default)
2. User
3. Device
4. Client
5. Agent
6. Credential
7. Request hint

Later defaults replace earlier defaults. Hard ceilings are intersected across
all layers and request hints cannot relax them. An explicit deny, an expired
deadline, or an empty model intersection rejects the request before routing.
Only one revision can be active for each tenant and scope target; activating a
new revision disables the prior revision in the same transaction and writes
both audit events.

Credential-scoped policies bind to the credential rotation family. A newly
rotated key therefore receives the same hard constraints during overlap and
later rotations. Creating a policy for any member normalizes its target to the
family root.

## Policy document

```json
{
  "defaults": {
    "workload_class": "interactive",
    "urgency": "immediate",
    "quality": "premium",
    "budget_class": "normal",
    "max_queue_time_ms": 30000
  },
  "constraints": {
    "max_quality": "standard",
    "max_budget_class": "normal",
    "allowed_models": ["company-model"]
  },
  "deny": false
}
```

Unknown fields, empty model lists, queue times outside `1..=86400000`, and deny
reasons longer than 256 bytes are rejected. A deny reason is valid only when
`deny` is true. Policy keys are limited to 200 characters and revisions are
positive integers.

The four dimensions remain independent:

| Dimension | Values |
| --- | --- |
| `workload_class` | `critical`, `interactive`, `foreground`, `automation`, `background`, `batch` |
| `urgency` | `immediate`, `normal`, `low`, `idle` |
| `quality` | `economy`, `standard`, `advanced`, `premium` |
| `budget_class` | `cheap`, `normal`, `unrestricted` |

Client or agent scoped policies are the configurable source defaults. Typical
starting revisions are:

| Source | Workload | Urgency | Quality |
| --- | --- | --- | --- |
| Claude Code | `interactive` | `immediate` | `premium` |
| VS Code completion | `interactive` | `immediate` | `economy` |
| Research agent | `background` | `low` | `premium` |
| Memory agent | `batch` | `idle` | `economy` |

These are examples, not hard-coded client-name checks. Administrators bind the
revision to the relevant Client or Agent identity.

## Request hints

The data plane accepts these headers:

- `x-aigw-workload-class`
- `x-aigw-urgency`
- `x-aigw-quality`
- `x-aigw-budget-class`
- `x-aigw-deadline-at` (RFC 3339)
- `x-aigw-max-queue-time-ms`

A `quality:<tier>` model selector is also a quality hint. Malformed hints return
`400 invalid_policy_hint`. Policy store or document failures return
`503 policy_unavailable`. Explicit denies return `403 policy_denied`; model
allowlist failures return `403 model_forbidden`. Policy headers are gateway-only
metadata and are not forwarded to providers.

`max_quality` constrains `quality:<tier>` selection. A caller using an explicit
public model ID is constrained by `allowed_models`; model quality labels are not
inferred from names. The deadline is checked again immediately before a
provider attempt, after model lookup and durable ledger reservation.

## Failure behavior

Policy lookups use a tenant-safe, bounded TTL cache. Admin policy mutations
invalidate that tenant immediately. A warm unexpired entry can serve a request
during a database outage; a miss or expired entry fails closed with
`policy_unavailable`. `POLICY_CACHE_CAPACITY` and `POLICY_CACHE_TTL_SECONDS`
default to `1024` entries and `60` seconds; both must be positive.

## File map

- `src/policy.rs`: typed documents, precedence, cache, and tenant lookup.
- `src/services/request_policy.rs`, `src/services/openai.rs`: request enforcement and durable rejection.
- `src/routes/messages.rs`: Anthropic HTTP extraction and response assembly.
- `src/admin/resources.rs`, `src/admin/reports.rs`: lifecycle, audit, and governance reads.
- `src/ledger.rs`: immutable request policy snapshot.
- `migrations/20260911051500_policy_engine.sql`: policy lifecycle and ledger schema.
- `web/src/pages.tsx`: policy operations and request decision display.

## Quota and rate limits

Active quota revisions are tenant scoped and may target the global tenant,
User, Device, Client, Agent, Credential rotation family, Model, or Provider.
Every matching scope is applied. A quota document is typed and contains at
least one of these limits:

```json
{
  "mode": "hard",
  "limits": {
    "requests_per_minute": 100,
    "estimated_tokens_per_minute": 500000,
    "tokens_per_hour": 10000000,
    "tokens_per_day": 100000000,
    "concurrent_requests": 20,
    "cost_per_day_microusd": 50000000,
    "cost_per_month_microusd": 1000000000
  }
}
```

Hard violations return `429 quota_exceeded`. Soft violations try lower quality
aliases in `premium`, `advanced`, `standard`, `economy` order; when no lower
compatible tier remains they return `429 quota_deferred`. The immutable
decision and exact violated quota are stored in
`gateway_requests.quota_decision`.

Reservations happen before any provider call. Text input uses UTF-8 byte count
as a conservative token upper bound. Output uses the request cap or the model
maximum, multiplied by OpenAI Chat `n`. Image input cannot be bounded by request
bytes and therefore fails closed whenever a token or cost quota applies.
Cost quotas require `token_v1` USD price metadata. Requests using cache controls
also require cache-read and cache-write rates; provider-billed tools or service
tiers fail closed until their price dimensions are represented. Anthropic
`count_tokens` consumes request-rate quota only, not inference token or cost
quota.

Persistent windows and reservations live in PostgreSQL. Provider-reported
usage appends a reconciliation event and adjusts estimated token/cost totals
exactly once under the same tenant lock as admission. Day and month boundaries
are fixed to UTC. `recompute_quota_windows(tenant_uuid)` rebuilds counters from
immutable reservations and reconciliation state under that lock.

Concurrency is a request-lifetime RAII lease. W12 deliberately supports one
gateway process; startup holds a PostgreSQL session advisory lock and a second
process fails before binding the listener. W19 must move this ownership into a
distributed durable design before enabling replicas.

## Rollout

Apply the migration before activating a policy. Create and inspect a draft,
then activate it through `POST /admin/v1/policies/{id}/lifecycle` with its
current version. Deploying a deny or model constraint should start at a narrow
scope and be confirmed in the Requests view before broadening it.
