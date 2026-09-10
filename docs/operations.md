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

## Usage ledger spool

Every accepted inference reserves two fixed-size files in `LEDGER_SPOOL_DIR`
before the provider is called. `LEDGER_SPOOL_MAX_BYTES` is a hard logical and
physical ceiling; when it is full, the gateway rejects the new request before
it can incur provider cost. One file preserves the interrupted state and the
other is a preallocated completion workspace, so finishing never needs new disk
space after provider cost. The default is 64 MiB with 32 KiB per file, allowing
1,024 reserved requests.

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

## Alerts

Pool capacity below reserve, spend velocity anomaly, repeated 401/403 from a
provider, quota-store inconsistency, fallback storm, p95 latency, 5xx rate,
audit write failure and secret rotation overdue.

## Fail closed

Authentication, model policy, budget reservation and audit durability fail
closed. Optional analytics may fail open. Provider routing fails with a clear
error when no approved deployment is healthy; it never falls back to an
unapproved model or credential source.
