# Operations

## Environments

Use separate development, staging and production databases, keys, provider
accounts and model aliases. A client key is valid in exactly one environment.

## Rollout

1. Threat model and provider-contract review.
2. Staging with synthetic accounts and hard spend caps.
3. Shadow policy evaluation without forwarding production content.
4. Pilot team with low budgets and one model class.
5. Canary production replicas, then measured expansion.

## Usage ledger migration

`20260910220000_usage_ledger.sql` is expand-only: it adds ledger tables, indexes,
constraints, append-only triggers, and identity deletion guards. Identity rows
must be revoked or deactivated instead of deleted so delayed ledger envelopes
cannot lose their foreign-key targets.
Deploy the migration before a binary that writes ledger envelopes.

Rollback the binary first and leave the ledger schema in place. Dropping these
tables is not an operational rollback because it destroys billing evidence. A
later corrective migration must be forward-only, preserve existing rows, and
backfill in one set-based SQL statement. Before any destructive replacement,
take and verify a PostgreSQL backup and document the restore command.

Deploy the migration-forward-compatible binary before any later release adds a
migration. It tolerates unknown newer migration versions while still validating
every migration embedded in that binary. This is what lets an older binary
restart after a rollback without deleting schema or migration history.

## Usage ledger spool

Every accepted inference reserves three fixed-size files in `LEDGER_SPOOL_DIR`
before the provider is called. `LEDGER_SPOOL_MAX_BYTES` is a hard logical and
physical ceiling; when it is full, the gateway rejects the new request before
it can incur provider cost. One file preserves the interrupted state and the
the other two are alternating preallocated checkpoint workspaces. The previous
complete checkpoint remains intact while the next is written, so a torn write
cannot erase all observed usage. The default is 64 MiB with 32 KiB per file,
allowing 682 reserved requests.

An envelope contains metadata only and is fsynced before a response is
considered accounted for. The container mounts the directory from the
`ledger-spool` named volume. Do not place it on ephemeral container storage.
This is a single-instance recovery mechanism, not a replicated ledger; back up
the volume together with PostgreSQL before moving hosts.

Each flush sends every currently ready envelope in one PostgreSQL function
call. After the first database failure, the process leaves all envelopes on
disk and makes no automatic retry loop. Restore PostgreSQL, then restart the
gateway once; startup applies migrations and performs one batched recovery
before accepting traffic.

## Process shutdown

`SIGTERM` and Ctrl-C start the same bounded shutdown sequence:

1. Mark the process as draining. New data-plane requests return `503` with
   `Retry-After`; `/ready` returns `503`, while `/health` remains `200` on the
   same listener.
2. Close scheduler admission and let requests that already own a lifecycle
   lease continue, including SSE response bodies.
3. When active requests reach zero, close the listener. If
   `DRAIN_TIMEOUT_MS` elapses first, cancel the process token and hard-close
   connections. Interrupted reservations are finalized as `cancelled` with
   `shutdown_timeout`, provider/request progress and separate estimated or
   partial provider-reported usage. Partial usage never lowers the outstanding
   quota reservation as if it were final.
4. Within `SHUTDOWN_FINALIZE_TIMEOUT_MS`, wait for response-body ownership to
   be released, durable ledger finalizers to finish and the spool worker to
   become idle. Spans and metrics then each get the same bounded flush window.

Set the deployment platform's termination grace period above
`DRAIN_TIMEOUT_MS + 3 * SHUTDOWN_FINALIZE_TIMEOUT_MS + 1 second`, plus an
operational margin. A shorter platform deadline can send `SIGKILL` before the
gateway's own bounds and durability path finish.

`REQUEST_HEADER_MAX_BYTES` is the aggregate application header limit and
returns `413`. The HTTP/1 parser has a fixed buffer at twice that value and
returns `431` before middleware when exceeded. Request bodies retain their
per-protocol limits. Provider DNS/connect/TLS setup is bounded by
`PROVIDER_CONNECT_TIMEOUT_MS`; provider headers and each later stream read are
bounded by `PROVIDER_IDLE_TIMEOUT_MS`. None of these paths retries a provider
request.

## Alerts

Pool capacity below reserve, spend velocity anomaly, repeated 401/403 from a
provider, quota-store inconsistency, fallback storm, p95 latency, 5xx rate,
audit write failure and secret rotation overdue.

## Fail closed

Authentication, model policy, budget reservation and audit durability fail
closed. Optional analytics may fail open. Provider routing fails with a clear
error when no approved deployment is healthy; it never falls back to an
unapproved model or credential source.
