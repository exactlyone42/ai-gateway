# Client compatibility

Compatibility is based on configuration surfaces, not on intercepting product
logins. Verify each pinned client version during implementation.

| Client | Preferred path | Status |
|---|---|---|
| Hermes | OpenAI-compatible base URL + virtual key | Planned verification |
| VS Code agents/extensions | Per-extension OpenAI-compatible provider setting | Planned matrix |
| Claude Code | Approved custom provider/API configuration only | Research required |
| Claude Desktop | Approved connector/API extension only; no subscription proxy | Research required |
| Codex CLI/Desktop | Custom provider/base URL when officially supported | Research required |
| Orca | OpenAI-compatible provider or documented adapter | Product identity/version required |

For clients that cannot set a custom endpoint, provide an explicit local Rust
adapter only when the client has a documented extension protocol. Do not use DNS
spoofing, TLS interception or session-token extraction.
