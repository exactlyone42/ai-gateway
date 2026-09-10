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
constraints, and append-only triggers without changing existing identity tables.
Deploy the migration before a binary that writes ledger envelopes.

Rollback the binary first and leave the ledger schema in place. Dropping these
tables is not an operational rollback because it destroys billing evidence. A
later corrective migration must be forward-only, preserve existing rows, and
backfill in one set-based SQL statement. Before any destructive replacement,
take and verify a PostgreSQL backup and document the restore command.

## Alerts

Pool capacity below reserve, spend velocity anomaly, repeated 401/403 from a
provider, quota-store inconsistency, fallback storm, p95 latency, 5xx rate,
audit write failure and secret rotation overdue.

## Fail closed

Authentication, model policy, budget reservation and audit durability fail
closed. Optional analytics may fail open. Provider routing fails with a clear
error when no approved deployment is healthy; it never falls back to an
unapproved model or credential source.
