# Passive provider routing

Each logical request makes exactly one upstream Provider call. The repository
rule against repeated remote operations prohibits same-request retries and
fallback calls, including hand-written second attempts. A transient failure can
change the route of the next request, but never replays the failed request.

## Decision order

1. Model Registry fetches all capability-compatible candidates in one database
   query.
2. Policy removes candidates the principal may not use.
3. The in-process Routing Engine takes one immutable snapshot and orders the
   remaining candidates by enabled passive signals.
4. Quota remains the sole authority for hard limits and reservations. When
   quota-aware routing is enabled, a soft limit may choose the next candidate.
5. Scheduler decides when the selected route can execute. It never changes the
   Provider or model.

The stable Registry order is the final tie-breaker. Unknown health/latency does
not displace a measured healthy route. Provider state is bounded to the profiles
loaded at startup. Every started request gets a monotonic Provider generation;
an older request completing late cannot overwrite a newer result.

## Switches

| Variable | Default | Effect |
| --- | --- | --- |
| `ROUTING_HEALTH_AWARE` | `true` | Prefer fewer transient failures, then lower measured latency |
| `ROUTING_LOAD_AWARE` | `true` | Prefer fewer active requests |
| `ROUTING_COST_AWARE` | `false` | Prefer lower declared token rates; unknown price sorts last |
| `ROUTING_QUOTA_AWARE` | `true` | Permit soft-quota routing; hard quota always remains enforced |
| `ROUTING_AUTO_QUALITY` | `true` | Permit passive signals to cross quality tiers |
| `ROUTING_FAILURE_COOLDOWN_MS` | `30000` | Half-open one failed route for reevaluation after this interval |

These switches affect ordering independently. Disabling quota-aware routing does
not disable quota enforcement; it changes a soft-limit outcome from routing to
deferred.

## Health and audit

HTTP 408, 500, 502, 503, 504, transport unavailability, body transport
failure, and explicit Provider failure are transient health signals. A completed
request clears the consecutive-failure score. Cancellation does not penalize a
Provider because it may be caused by the client. HTTP 429 is returned to the
caller but does not open the Gateway circuit: the Provider owns its rate limit,
and one rejection must not turn later requests into Gateway-generated 503s.

The durable policy snapshot stores `routing.attempt_kind=initial`, the selected
reason (`registry`, `health`, `load`, `cost`, `quota`, or `quality`), and
`same_request_retry=false`. Admin Request Detail exposes this as Routing History.
There is exactly one Provider attempt for each request; no stream reconnect,
usage lookup, timeout, tool continuation, or ambiguous result makes a second
remote call.
