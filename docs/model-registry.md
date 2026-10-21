# Model Registry Operations

Production tenants route only through active rows in PostgreSQL. `ALLOWED_MODELS`
is limited to the nil-tenant development bootstrap and is never a production
fallback.

`PROVIDER_ALLOWED_TENANT_IDS` explicitly authorizes which tenants may use the
server-side provider credentials. A database Admin cannot activate a matching
provider tuple for another tenant. Nil tenant IDs are rejected by Admin
bootstrap, credential issuance, and registry tables.

## Resources

- `POST /admin/v1/providers` creates a draft provider. Its `provider_key`,
  `base_url`, `credential_ref`, and `supported_operations` must exactly match a
  server-side provider profile before activation.
- `POST /admin/v1/models` creates a draft public-to-provider model mapping with
  capabilities, supported wire operations, context limits, and cost metadata.
- `POST /admin/v1/model-aliases` creates a protocol-scoped quality selector.
  Valid selectors are `economy`, `standard`, `advanced`, and `premium`.
- Generic `GET`, `PATCH`, and draft `DELETE` operations use the same resource
  paths and optimistic `version` contract as the other Admin resources.

## Lifecycle

Only an Admin can call:

```text
POST /admin/v1/{providers|models|model-aliases}/{id}/lifecycle
{"version":1,"state":"active"}
```

Draft and disabled provider/model/alias rows may be edited with version CAS and
then reactivated. Activate in dependency order: provider, model, then alias. Provider activation
fails unless its origin, credential reference, provider key, and every wire
operation match an immutable server-side profile. Model activation requires an
active provider. Alias activation requires an active model/provider and a wire
operation supported by both that provider and model.

Use the same endpoint with `state: "disabled"` to remove a row from routing.
Disabling a model also disables its active aliases. Disabling a provider also
disables its active models and aliases in the same transaction. Requests that
begin selection after commit observe the new state; already selected in-flight
requests are not cancelled.

Activating an alias atomically replaces the active alias for the same tenant,
operation, and quality tier. Registry lifecycle transitions are serialized per
tenant, and cascaded model/alias changes receive their own audit rows.

## Request Contract

Clients may select a public model ID or `quality:<tier>`. Selection is scoped by
tenant and wire operation, ordered deterministically, and rejects unsupported
streaming, tools, reasoning, image, or structured-output requirements. The
gateway may rewrite only the top-level `model` value before forwarding.

Public and provider model IDs are at most 255 bytes, descriptions 1024 bytes,
latency classes 64 bytes, and cost metadata 4096 serialized bytes. The entire
`quality:` namespace is reserved for aliases. These bounds keep the initial
durable ledger reservation below its fixed spool slot before any provider call.

Every production selection stores selector, alias/model/provider IDs and
versions, provider model ID, and cost metadata in the durable usage envelope.
Historical usage therefore keeps the exact registry state used at request time.
The initial reservation already contains this snapshot, so restart recovery
does not lose it after a hard process stop.

`GET /v1/models` returns the standard OpenAI `data[]` shape from active registry
rows. Untrusted provider profiles are not advertised. Codex CLI 0.153.0 does not
query this endpoint for custom providers; its startup metadata comes from the
local `model_catalog_json` path. The gateway therefore does not emit an
undocumented private catalog or invent reasoning levels that are absent from the
registry.
