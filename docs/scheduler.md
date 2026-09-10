# Scheduler and admission

The scheduler answers **when** a request may use an upstream. Model selection
continues to answer **where** in `model_registry`; the scheduler receives the
already selected provider/model and never changes either one.

## Admission order

Authentication, effective policy, model permission, budget/quality policy and
durable quota reservation run first. A durable ledger envelope is then created.
The scheduler atomically acquires global, provider, model, user, device, client,
agent, credential, workload and quota-concurrency capacity before the provider
call. A request that cannot run immediately checkpoints `queue_entered_at`
before becoming a waiter and checkpoints `queue_left_at` before calling the
provider. The effective scheduler deadline is persisted and checked again after
the queue-left fsync. Cancellation, process recovery and pre-provider rejection
reconcile token and cost reservations to zero; request-rate quota remains consumed.

The scheduler produces `accept`, `queue` and `reject`. W12 model selection owns
quota-driven `downgrade`; W17 routing owns `route elsewhere`. The request
admission flow composes these outcomes without moving routing into the scheduler.

## Fairness and bounds

Smooth weighted-fair selection uses fixed weights: critical 20, interactive 10,
foreground 6, automation 3, background 2 and batch 1. Each runnable class gains
credit by its effective weight; the selected class spends the sum of runnable
weights. Every aging interval raises a waiting class's effective weight by one,
capped at 20. Within one workload, the earliest bounded protocol cache expiry
wins before entry sequence; a request without cache affinity sorts last.

Critical/interactive/foreground share the interactive family. The other three
classes share the background family. Each family has execution and queue
reservations and may use shared capacity, but cannot borrow the other family's
reservation. Tenant and credential limits are isolated by family. When a family
queue is full, a new tenant or credential may replace the newest same-workload
waiter from a caller already holding multiple cells; the replaced waiter receives
the normal queue-full response. This bounded fair-share rule avoids claiming that
a finite queue can reserve cells for an unlimited unknown tenant population. A
blocked queue head is skipped when another request can atomically acquire all of
its dimensions.

Hard quota concurrency rejects immediately with `quota_exceeded`; soft quota
concurrency waits for capacity. The quota decision snapshot keeps each
constraint's id, mode and limit, and records a hard scheduler rejection.

Defaults are 12 interactive-reserved, 4 shared and 4 background-reserved active
slots. Queue defaults are 64/128/64, with 64 per tenant and 32 per credential.
`SCHEDULER_*` variables in `.env.example` configure every bound. The process hard
queue timeout defaults to 30 seconds and cannot be relaxed by a request hint.
`SCHEDULER_AGING_ENABLED` and `SCHEDULER_RESERVATIONS_ENABLED` exist for W18
negative controls; production keeps both enabled.

On shutdown `close()` rejects queued requests and stops admission while active
leases drain. A lease is held by the durable reservation, including for the full
lifetime of an SSE response body.

## Visibility

The Admin Overview combines tenant-scoped durable queue P50/P95 and
rejection/expiry counts with a tenant-filtered runtime snapshot. Governance and
System Health expose only that tenant's active/queued counts, workload and quality
usage, provider concurrency and cache-affinity risk. Cache reads and writes remain
separate usage dimensions in the ledger and Admin reports. The repository does
not contain the specification section 57 metric names; the engineering standard
forbids inventing OTLP aliases, so adding their separate OTLP instruments remains
blocked on that contract rather than shipping unstable names.
