# Provider Protocol Contracts

## Anthropic Messages

The gateway exposes and forwards these endpoints directly to the configured
Anthropic base URL; LiteLLM and canonical conversion are not involved:

- `POST /v1/messages`
- `POST /v1/messages/count_tokens`
- `GET /v1/models` is served from `ALLOWED_MODELS` and does not call upstream.
- `HEAD /api/hello` returns 204 for Claude Code prewarming.

### Request contract

- Preserve the original path, ordered query string, and JSON body bytes.
- Forward `content-type`, `accept`, and every `anthropic-*` header unchanged.
- Never forward the client `authorization` or `x-api-key`; inject only the
  server-side `ANTHROPIC_API_KEY` as upstream `x-api-key`.
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
