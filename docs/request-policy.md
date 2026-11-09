# Request policy

The gateway evaluates one immutable policy snapshot before model selection and
before any provider request. The snapshot is stored in
`gateway_requests.policy_decision`; later policy edits cannot change an
in-flight or historical decision.

## Resolution order

Active policies are tenant scoped and resolve from broadest to narrowest:

1. Global (the tenant/company default)
2. Group
3. User
4. Device
5. Client
6. Agent
7. Credential
8. Request hint

Later defaults replace earlier defaults. Hard ceilings are intersected across
all layers and request hints cannot relax them. An explicit deny, an expired
deadline, or an empty model intersection rejects the request before routing.
群組與個人政策同時生效：個人預設值可取代群組預設值，但不能取消群組的禁止或硬限制。
使用者屬於多個群組時，群組政策依群組 ID 排序；相同預設欄位由排序較後的群組決定。群組 ID 是自動產生的識別碼，不代表管理者指定的優先順序，因此應避免在多個群組設定互相衝突的預設值。
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

Quality tiers are compatibility aliases for callers and policy limits, not a
universal rating of model intelligence. Automatic routing first removes models
that cannot perform the requested operation or capability. Jev prioritizes the
remaining models using their declared purpose, capability flags, context,
latency class, and pricing metadata; the router then applies live provider
health, latency, load, and cost. If Jev cannot decide, the caller's original
model is preserved. The Admin catalog separately shows 30-day measurements.

`max_quality` constrains `quality:<tier>` selection. A caller using an explicit
public model ID is constrained by `allowed_models`; model quality labels are not
inferred from names. The deadline is checked again immediately before a
provider attempt, after model lookup and durable ledger reservation.

## Failure behavior

Policy lookups without a user use a tenant-safe, bounded TTL cache. Lookups with a
user always read the database so membership changes made by another process are
visible. Admin policy mutations invalidate that tenant immediately. A warm
unexpired entry without a user can serve a request during a database outage; a
lookup with a user, a miss or expired entry fails closed with
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
Group, User, Device, Client, Agent, Credential rotation family, Model, or Provider.
同名個人額度只有在模式、模型和供應商相同，且涵蓋群組的全部有效限制項目時，才取代該群組額度；空值不算有效限制。其他命中的額度仍會套用。
被取代的群組額度不會檢查，也不會記入該群組共用額度；因此管理者建立個人額度時，可以為該使用者設定比群組更寬的例外。要維持所有成員共用的上限，請避免建立會取代它的同名個人額度。
A quota document is typed and contains at
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

同批有多筆請求結帳時，先按租戶、額度、限制種類及起算時間加總調整量，再更新每個額度視窗一次。用量與成本都依各筆預留和最終實報的差額調整；重播不會再次調整，混入衝突內容時整批拒絕。

`20261110000001_quota_batch_windows.sql` 修正後續入帳，不會自動改寫歷史額度。升級前已有的錯帳仍須另外核對；重建計數要沿用既有租戶鎖與 `recompute_quota_windows`，不能把原事件或驗收原件改成預期數字。這次選擇修正原入帳函數，保留外層重播檢查，避免新增另一套結帳路徑。

Concurrency is a request-lifetime RAII lease. W12 deliberately supports one
gateway process; startup holds a PostgreSQL session advisory lock and a second
process fails before binding the listener. W19 keeps that contract: enabling
replicas is separate measured work that must first replace in-memory limits
with cross-instance durable counters.

## Rollout

Apply the migration before activating a policy. Create and inspect a draft,
then activate it through `POST /admin/v1/policies/{id}/lifecycle` with its
current version. Deploying a deny or model constraint should start at a narrow
scope and be confirmed in the Requests view before broadening it.
