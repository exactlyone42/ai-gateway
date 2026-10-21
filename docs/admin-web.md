# Admin Web Contract

The original section 48 metric names and section 92 question list are not present in this
repository. This document is the testable W09 local contract; it does not claim to reconstruct
missing upstream text.

## Ten overview metrics

All values use one tenant-scoped `GET /admin/v1/overview?from=<inclusive>&to=<exclusive>` query.
Usage selects one canonical row per provider attempt: `provider_reported` first, otherwise
`estimated`. Both Overview and Usage attribute that canonical row to the gateway request's
`created_at`, so a late provider report replaces its estimate without moving usage between periods.

| UI label | API field | Source | Empty behavior |
| --- | --- | --- | --- |
| Requests | `requests` | gateway requests | 0 |
| Success rate | `success_rate` | completed / requests | unavailable when no requests |
| Unsuccessful | `unsuccessful_requests` | non-completed requests | 0 |
| Input tokens | `input_tokens` | canonical usage | 0 |
| Output tokens | `output_tokens` | canonical usage | 0 |
| Total tokens | `total_tokens` | canonical input + output | 0 |
| Cache read | `cached_input_tokens` | canonical usage | 0 |
| Cache write | `cache_write_input_tokens` | canonical usage | 0 |
| P95 Gateway processing | `p95_gateway_overhead_ms` | Gateway request processing plus first response processing, excluding queue and provider time | unavailable when absent |
| Cost | `cost_microusd` | canonical usage cost | unavailable with reason until pricing is active |

Queue state is adjacent operational status, not an invented eleventh metric. W13 supplies
tenant-scoped runtime active/queued state plus durable P50/P95, rejected and expired values.

## Sixteen operator questions

| # | Question | Page and evidence |
| --- | --- | --- |
| 1 | Is request traffic arriving? | Overview `requests` |
| 2 | What share succeeds? | Overview `success_rate` |
| 3 | How many requests are unsuccessful? | Overview `unsuccessful_requests` |
| 4 | Is Gateway processing time degrading? | Overview `p95_gateway_overhead_ms` |
| 5 | Is provider first-token latency degrading? | Request/provider attempt detail |
| 6 | How many tokens are consumed? | Overview totals and Usage groups |
| 7 | How much usage is estimated? | Overview and Usage `unreconciled_attempts` |
| 8 | What does usage cost? | Cost or an explicit unavailable reason |
| 9 | Which user/device/client/agent used the gateway? | Identities paths and Usage dimensions |
| 10 | Which credential was involved? | Credential UUID/prefix and request principal |
| 11 | Which model and provider handled a request? | Requests detail and Catalog |
| 12 | What provider attempts are persisted? | Request detail, ordered by attempt number |
| 13 | Can retry/fallback history be inspected? | Routing History shows the single initial attempt and durable pre-call reason; same-request retry is explicitly disabled |
| 14 | What policy, quota and scheduler state is active? | Active policy and quota revisions; current quota windows and UTC resets; tenant-scoped scheduler runtime |
| 15 | Are gateway, database and telemetry configured? | System Health component status |
| 16 | Who changed control-plane state? | Tenant-scoped, redacted Audit rows |

Team reporting is unavailable because no team membership exists in the Principal or ledger schema.
Full spans remain in the configured OTLP backend and are not represented as gateway-persisted data.

## Evidence for improvement claims

Request detail stores two append-only parameter snapshots: `requested` is what the client asked
for and `provider` is what the gateway actually sent. The allowlist contains model, output limit,
sampling values, reasoning mode, streaming flags, and counts or lengths for messages, text, tools,
images, and structured output. It never contains prompt or response text, system instructions,
tool names or arguments, headers, or credentials. Each snapshot is limited to 4 KiB by PostgreSQL.

An improvement claim must compare fixed time windows or a controlled A/B cohort and report sample
size alongside the result. Reliability uses completed requests divided by all terminal requests;
latency reports median and P95 separately; cost uses provider-reported usage when available and
must state the unreconciled count. Model-routing savings require a priced baseline model and the
same workload cohort. The gateway must not call an estimate a saving when either price, usage, or
quality acceptance is missing. Parameter snapshots explain whether a result followed a changed
model or generation setting; they do not prove response quality by themselves.

## Browser security

The SPA is served from `/admin/` by the gateway. API routes under `/admin/v1` take precedence.
Static responses have a CSP that permits only same-origin scripts, styles, images, fonts, forms and
connections. The short session token exists only in React memory and is cleared on 401, expiry or
logout; browser storage is not used. Credential plaintext is held only in local modal state and is
discarded when the modal closes. It is never placed in TanStack Query data.

Viewer mutation controls are not rendered. This is only a convenience: the API remains the security
boundary and responds 403 while writing a denial audit row.
