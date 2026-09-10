# Client roundtrip evidence

Verified 2026-09-11 with Claude Code 2.1.267 and Codex CLI 0.153.0. The harness
starts one local fixture provider and one local gateway, invokes each real client
once, and makes no request to a production provider.

## Replay

Use a disposable PostgreSQL database. The script does not retry database or
service connections; a failed startup or health check fails the run.

```bash
CLIENT_ROUNDTRIP_ALLOW_DATABASE_MUTATION=1 \
  DATABASE_URL=postgres://user:password@127.0.0.1:5432/disposable \
  ./scripts/verify-client-roundtrip.sh all
```

Expected terminal result:

```text
client roundtrip ok: all
sanitized captures: /tmp/apg-client-roundtrip.<random>/captures
```

The client stdout assertions are exact:

- Claude Code: `W05_CLAUDE_CODE_OK`
- Codex CLI: `W06_CODEX_OK`

## Preserved captures

- `tests/fixtures/client-roundtrip/captures/claude-code-2.1.267.json`
- `tests/fixtures/client-roundtrip/captures/codex-cli-0.153.0.json`

Captures retain only path, query names without values, model, stream mode, tool
count, reasoning/thinking presence, selected header names, and booleans proving
the fixture provider credential replaced the client credential. They never retain
credential values, prompt or response content, full headers, request bodies, user
IDs, session IDs or agent IDs. The provider fixtures are `anthropic-response.sse` and
`openai-response.sse` in the parent directory.

## Evidence boundary

This proves the named local client versions can load a custom endpoint, send a
request through this gateway, parse the fixture SSE response and print the exact
terminal text. It does not prove production provider credentials, production
models, billing, every tool call/result shape, or future client versions.
The harness refuses a different client version until its capture is reviewed and
the versioned evidence is updated. It also refuses a database name that does not
contain `test`, `validation`, or `disposable`, and requires the explicit mutation
flag because gateway startup runs migrations.

For Codex 0.153.0 the harness also locks the observed custom-provider metadata
boundary: the client emits its unknown-model fallback warning and does not call
the gateway `/v1/models` endpoint. Codex documents `model_catalog_json` as a
startup-loaded local path. The gateway cannot remove that warning over its HTTP
provider contract and does not fabricate a private catalog.

Tool counts are observations, not capability contracts. Claude Code exposed 54
tools in this run; isolated Codex exposed 9. Earlier Codex runs exposed 17. The
contract therefore records serialization and a nonzero list, not a fixed count.
