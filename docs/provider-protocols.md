# Provider Protocol Contracts

## Anthropic Messages

The gateway exposes and forwards these endpoints to the configured Anthropic
base URL. The protocol adapter creates a canonical semantic view, while a
short-lived native wire sidecar preserves the same-dialect request bytes;
LiteLLM is not involved:

- `POST /v1/messages`
- `POST /v1/messages/count_tokens`
- `GET /v1/models` is served from the tenant's active database registry and does not call upstream.
- `HEAD /api/hello` returns 204 for Claude Code prewarming.

### Request contract

- Preserve the original path and ordered query string. The router may replace only the top-level
  JSON `model` value with the selected provider model; all other request semantics are preserved.
- Forward `content-type`, `accept`, and every `anthropic-*` header unchanged.
- Never forward the client `authorization` or `x-api-key`; inject only the
  server-side `ANTHROPIC_API_KEY` as upstream `x-api-key`.
- The target provider adapter owns the native endpoint, authentication mode,
  and request-header allowlist. A shared transport sends the prepared request;
  client routes do not select provider authentication or upstream paths.
- Client authentication accepts Bearer or `x-api-key`. If both are present,
  they must name the same secret.

### Response contract

- Preserve status and body bytes. Forward `content-type`, `content-encoding`,
  `retry-after`, `request-id`, and every `anthropic-*` response header.
- Stream SSE bytes incrementally, including comments, pings, CRLF/CR-only line
  endings, and unknown events. Accounting observes a bounded copy and never
  rewrites the wire bytes.
- `message_stop` is the protocol terminator. It is relayed before the gateway
  closes the upstream body. Transport EOF without `message_stop` is an error.
- A client TCP disconnect drops the upstream response body and closes the
  provider connection. No retry occurs after streaming has started.

### Accounting contract

- Estimated input usage and provider-reported usage are separate records; they
  are not summed or used to overwrite each other.
- Provider response ID and cumulative usage are checkpointed before bytes that
  reveal them are handed to the client.
- Anthropic token fields map as follows:

| Anthropic field | Ledger field |
| --- | --- |
| `input_tokens` | `input_tokens` |
| `output_tokens` | `output_tokens` |
| `cache_read_input_tokens` | `cached_input_tokens` |
| `cache_creation_input_tokens` | `cache_write_input_tokens` |

The gateway does not claim that these token units are interchangeable with any
other provider. Cross-provider normalization belongs to W14-W16.

Sources: Anthropic Messages API and Claude Code LLM gateway protocol, verified
2026-09-08 and linked with exact compatibility boundaries in
`docs/feasibility.md`.

## OpenAI Responses and Chat Completions

The gateway exposes and forwards these endpoints to `OPENAI_BASE_URL`. The
protocol adapter creates a canonical semantic view, while a short-lived native
wire sidecar preserves the same-dialect request bytes:

- `POST /v1/responses`, used by Codex CLI.
- `POST /v1/chat/completions`, used by other OpenAI-compatible clients.
- `GET /v1/models` is served from the tenant's active database registry and does not call upstream.

### Request and response contract

- Preserve the original path and ordered query string. The router may replace only the top-level
  JSON `model` value with the selected provider model; all other request semantics are preserved.
- Forward `content-type`, `accept`, `openai-beta`, `originator`, `session_id`,
  and `x-stainless-*` headers. Never forward client `authorization` or
  `x-api-key`; inject only the server-side `OPENAI_API_KEY` bearer token.
- The target provider adapter owns the native endpoint, authentication mode,
  and request-header allowlist. A shared transport sends the prepared request.
- Preserve upstream status and body bytes. Forward `content-type`,
  `content-encoding`, `retry-after`, `request-id`, `x-request-id`, and
  `openai-*` response headers.
- Upstream 4xx/5xx preserve the provider status and body bytes; the provider
  chooses that body's shape. Gateway-owned errors use a stable `error.code`.

### Streaming and accounting contract

- The shared relay frames one complete bounded SSE event without rewriting its
  bytes. Provider observers emit `CanonicalStreamEvent` values for lifecycle,
  response ID, usage, completion hints, and failures.
- The canonical event vocabulary defines ordered message/content starts and
  completions, text and reasoning deltas, tool-call argument deltas, one
  response completion event, and one canonical error event. The accounting
  relay consumes those same terminal events. Native forwarding does not
  synthesize semantic deltas; W15 activates them only when a response crosses
  protocol dialects.
- Responses requires an explicit `response.completed`. `response.failed`,
  `response.incomplete`, or `error` fails the ledger and the response body.
- Chat Completions accepts `[DONE]` after `finish_reason`, or clean EOF after
  `finish_reason`; late usage between them is retained. EOF before a finish
  reason is an error.
- Malformed JSON, an event field without `data:`, or an event above
  `STREAM_EVENT_MAX_BYTES` fails the stream. The default is 32 MiB and can be
  raised for providers that place large tool results in one SSE event. Terminal
  bytes are not released until the durable ledger finish succeeds.
- Dropping the downstream body drops the owned upstream response body, closing
  the provider connection. No claim is made that provider billing stops.
- Estimated and provider-reported usage remain separate. Responses and Chat
  token fields map to input, output, cached input, and reasoning ledger fields.

Codex CLI 0.153.0 was exercised against this contract on 2026-09-11. It sent a
streaming Responses request and consumed an explicit `response.completed`
event. See `docs/client-compatibility.md` for the observed client fields and
the model metadata fallback boundary.

Sources: OpenAI Responses create and streaming event references, and the Codex
configuration reference, verified 2026-09-11 and linked in
`docs/client-compatibility.md` and `docs/feasibility.md`.
