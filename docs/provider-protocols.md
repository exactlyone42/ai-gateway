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
- Forward `content-type`, `accept`, every `anthropic-*` header, and Claude
  Code's `x-claude-code-session-id`, `x-stainless-retry-count`, and
  `x-cc-original-request-id` retry identity headers unchanged.
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
- `STREAM_HEARTBEAT_INTERVAL_MS` defaults to 30 seconds and must stay below 60
  seconds. The gateway emits an SSE comment while waiting for Anthropic
  response headers and during later provider silence, so clients do not need
  the provider itself to keep the downstream connection alive. If the provider
  rejects after the gateway has opened that stream, the rejection is delivered
  as an Anthropic SSE error because the HTTP status is already committed.
- `PROVIDER_IDLE_TIMEOUT_MS` limits time between successful upstream reads and
  defaults to 120 seconds. Each successful read resets the timeout, so an
  active long-running stream is not capped by total lifetime. An idle timeout
  closes the attempt and returns the gateway's upstream failure response; it
  never retries the same logical request.
- `PROVIDER_CONNECT_TIMEOUT_MS` separately limits provider DNS, TCP connect and
  TLS setup, and defaults to 10 seconds. It applies to every shared provider
  transport path, including health probes, and never causes an automatic retry.
- Estimated and provider-reported usage remain separate. Responses and Chat
  token fields map to input, output, cached input, and reasoning ledger fields.

Codex CLI 0.153.0 was exercised against this contract on 2026-09-11. It sent a
streaming Responses request and consumed an explicit `response.completed`
event. See `docs/client-compatibility.md` for the observed client fields and
the model metadata fallback boundary.

Sources: OpenAI Responses create and streaming event references, and the Codex
configuration reference, verified 2026-09-11 and linked in
`docs/client-compatibility.md` and `docs/feasibility.md`.

## Cross-provider translation

W15 deliberately exposes a narrow intersection instead of claiming general
OpenAI/Anthropic compatibility. A neutral canonical operation finds candidate
models, but it does not authorize a target call. Before quota reservation, the
gateway builds a validated translation plan and checks that the model row,
provider row, and trusted runtime profile all authorize the exact target
operation.

| Source | Target | Text | Stream | Client tools | JSON Schema | Reasoning |
| --- | --- | --- | --- | --- | --- | --- |
| Anthropic Messages | OpenAI Responses | translate | reject before quota | translate | verified subset | reject 400 |
| OpenAI Responses | Anthropic Messages | translate | strict state machine | translate | verified subset | reject 400 |
| OpenAI Chat Completions | Anthropic Messages | translate | strict state machine | translate | reject | reject 400 |
| Anthropic Count Tokens | OpenAI | reject | reject | reject | reject | reject |

Chat Completions to Anthropic accepts `messages`, `tools`, `tool_choice`,
`max_tokens` or `max_completion_tokens` (required, no default is invented),
`stream`, `temperature` (0 to 1; a larger value is rejected instead of being
clamped), `n` only when it equals 1, and `stream_options.include_usage`. The
fields `reasoning_effort`, `top_p`, `stop`, `user`, `seed`, `frequency_penalty`
and `presence_penalty` are accepted and dropped, never sent upstream, so a
harness that always sends them keeps working. Every other field, including the
`developer` role, is rejected with 400. Consecutive `tool` messages become one Anthropic user message
holding one `tool_result` per call. The response uses `finish_reason`
`tool_calls`, `stop` or `length` for `tool_use`, `end_turn` or `max_tokens`, and
reports `prompt_tokens` as input plus cached plus cache-write tokens. Streaming
sends a role chunk, tool-call deltas, one chunk with `finish_reason`, an optional
usage chunk when `include_usage` is true, then `data: [DONE]`. `n` greater than 1
stays unsupported because Anthropic returns one choice only.

The translated request scanner covers root options, messages, content blocks,
tool definitions, tool calls/results, structured-output schema, and reasoning.
It rejects images, opaque blocks, unknown roles, unmatched or duplicate tool
call IDs, malformed tool arguments, `json_object`, unsupported schema keywords,
provider extensions, input-level system/developer messages whose position would
change, background/conversation state, and every explicit reasoning or thinking
setting/history block. Generated Responses function-call items can be supplied
to the next request without losing their public item ID or semantic call ID. No
rejected candidate enters the quota engine or reaches a provider.

Translated requests forward only neutral `content-type` and `accept` headers.
Target authentication and `anthropic-version` come from the trusted adapter;
client vendor headers never cross providers. Translated responses rebuild
headers and protocol bodies, use gateway-namespaced public response IDs, and
keep target response IDs only in the ledger. Provider-reported target usage is
the accounting authority even when response transcoding later fails. A target
token limit returns the partial translated response with the source protocol's
incomplete marker and records the request/attempt as failed incomplete.

Native same-dialect traffic retains the byte-preserving W05/W06 behavior. Its
observer tolerates unknown events and forwards raw bytes. A translated stream
instead fails closed on unknown semantic events and validates start/delta/stop
order, unique indexes and terminal events, bounded item/tool-argument counts,
and total provider/client response bytes.

Streaming translation is intentionally directional. OpenAI Responses reports
input usage only in its terminal event, but Anthropic requires input usage in
`message_start`; therefore Anthropic-source to OpenAI-target streaming is
rejected before quota. OpenAI-source to Anthropic-target streaming is accepted
because Anthropic supplies input usage at `message_start`. Non-empty citations,
annotations, and initial streamed tool input are rejected when they cannot be
represented without loss. Responses envelopes echo the accepted source request
fields, including `instructions`, from one immutable request snapshot.

The feature is a gateway API capability, not a claim that Claude Code supports
non-Claude routing. Anthropic's Claude Code gateway policy explicitly excludes
that use. Protocol shapes were checked against the Anthropic streaming,
structured output, stop-reason, and strict tool-use documentation and the
OpenAI Responses streaming reference on 2026-09-11.

## Gemini, xAI, vLLM, and Ollama

These providers use the same required adapter contract: typed capabilities, an executable
one-shot health probe, buffered send, stream send, usage mapping, and integer cost estimation.
`EXTRA_PROVIDER_PROFILES_JSON` selects the provider kind explicitly; the gateway never guesses
vLLM versus Ollama from a URL. Each entry names a `secret_env`; the JSON must not contain a secret.
Gemini and xAI require one. Local profiles may omit it, while a configured local secret is sent as
an optional Bearer token. Client authorization headers are always removed.

| Kind | Request and stream endpoint | Health endpoint | Usage availability |
| --- | --- | --- | --- |
| Gemini | `/v1beta/models/{model}:generateContent`; stream uses `streamGenerateContent?alt=sse` | `/v1beta/models/{model}` | required |
| xAI | OpenAI-compatible `/v1/responses` or `/v1/chat/completions` | `/v1/models` | required |
| vLLM | OpenAI-compatible Responses or Chat | `/health` | optional, model/server dependent |
| Ollama | OpenAI-compatible Responses or Chat | `/api/version` | optional, version/model dependent |

Gemini uses `x-goog-api-key`; xAI uses Bearer. Gemini text, JSON schema output, terminal usage,
and text SSE are translated. Tool and reasoning requests are rejected before the provider call
because the current response decoder cannot preserve Gemini `functionCall` or thought content.
Unsupported part types, malformed events, unknown finish reasons, missing terminal usage, and
events after terminal completion fail closed.

Token fields are semantic mappings, not a common unit and not values that may be directly added:

| Provider field | Canonical field | Relationship |
| --- | --- | --- |
| Gemini `promptTokenCount - cachedContentTokenCount` | `input_tokens` | uncached prompt tokens |
| Gemini `cachedContentTokenCount` | `cached_input_tokens` | subset/detail of prompt tokens |
| Gemini `candidatesTokenCount` | `output_tokens` | generated candidate tokens |
| Gemini `thoughtsTokenCount` | `reasoning_tokens` | separate thinking detail reported by Gemini |
| xAI/OpenAI-compatible input total minus cached/write details | `input_tokens` | uncached input tokens |
| xAI/OpenAI-compatible cached-token detail | `cached_input_tokens` | subset/detail, not extra input |
| xAI/OpenAI-compatible output total minus reasoning detail | `output_tokens` | non-reasoning output tokens |
| xAI/OpenAI-compatible reasoning detail | `reasoning_tokens` | output detail, not extra output |

Local providers may omit usage. The ledger records that absence as no provider usage record; it
does not turn missing usage into an authoritative zero. Pricing still comes from the selected
model's versioned `cost_metadata`; provider token counts are never converted across tokenizers.

Sources, verified 2026-09-11: [Gemini generateContent and usage metadata](https://ai.google.dev/api/generate-content),
[xAI OpenAI-compatible inference](https://docs.x.ai/developers/rest-api-reference/inference),
[xAI usage and pricing fields](https://docs.x.ai/developers/advanced-api-usage/prompt-caching/usage-and-pricing),
[vLLM OpenAI-compatible server](https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server/),
[vLLM stream usage](https://docs.vllm.ai/en/latest/features/per_request_metrics/), and
[Ollama OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility).
