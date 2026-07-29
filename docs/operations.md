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

## Alerts

Pool capacity below reserve, spend velocity anomaly, repeated 401/403 from a
provider, quota-store inconsistency, fallback storm, p95 latency, 5xx rate,
audit write failure and secret rotation overdue.

## Fail closed

Authentication, model policy, budget reservation and audit durability fail
closed. Optional analytics may fail open. Provider routing fails with a clear
error when no approved deployment is healthy; it never falls back to an
unapproved model or credential source.
