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
| Cached input | `cached_input_tokens` | canonical usage | 0 |
| P95 latency | `p95_latency_ms` | completed request latency | unavailable when absent |
| P95 TTFT | `p95_ttft_ms` | provider attempt TTFT | unavailable when absent |
| Cost | `cost_microusd` | canonical usage cost | unavailable with reason until pricing is active |

Queue state is adjacent operational status, not an invented eleventh metric. It remains
unavailable until W13 supplies scheduler telemetry.

## Sixteen operator questions

| # | Question | Page and evidence |
| --- | --- | --- |
| 1 | Is request traffic arriving? | Overview `requests` |
| 2 | What share succeeds? | Overview `success_rate` |
| 3 | How many requests are unsuccessful? | Overview `unsuccessful_requests` |
| 4 | Is end-to-end latency degrading? | Overview `p95_latency_ms` |
| 5 | Is provider first-token latency degrading? | Overview `p95_ttft_ms` |
| 6 | How many tokens are consumed? | Overview totals and Usage groups |
| 7 | How much usage is estimated? | Overview and Usage `unreconciled_attempts` |
| 8 | What does usage cost? | Cost or an explicit unavailable reason |
| 9 | Which user/device/client/agent used the gateway? | Identities paths and Usage dimensions |
| 10 | Which credential was involved? | Credential UUID/prefix and request principal |
| 11 | Which model and provider handled a request? | Requests detail and Catalog |
| 12 | What provider attempts are persisted? | Request detail, ordered by attempt number |
| 13 | Can retry/fallback history be inspected? | Explicit unavailable state until W17 |
| 14 | What policy, quota and scheduler state is active? | Active policy and quota revisions; current quota windows and UTC resets; scheduler remains unavailable until W13 |
| 15 | Are gateway, database and telemetry configured? | System Health component status |
| 16 | Who changed control-plane state? | Tenant-scoped, redacted Audit rows |

Team reporting is unavailable because no team membership exists in the Principal or ledger schema.
Full spans remain in the configured OTLP backend and are not represented as gateway-persisted data.

## Browser security

The SPA is served from `/admin/` by the gateway. API routes under `/admin/v1` take precedence.
Static responses have a CSP that permits only same-origin scripts, styles, images, fonts, forms and
connections. The short session token exists only in React memory and is cleared on 401, expiry or
logout; browser storage is not used. Credential plaintext is held only in local modal state and is
discarded when the modal closes. It is never placed in TanStack Query data.

Viewer mutation controls are not rendered. This is only a convenience: the API remains the security
boundary and responds 403 while writing a denial audit row.
