# Client 相容性

相容性只建立在**官方支援的設定介面**上。明確不做：TLS 中間人、憑證注入、DNS 欺騙、
破解 Client、改執行檔、攔截訂閱憑證。某個 Client 沒有官方路徑就標 `unsupported`／`pending`，
**不硬 hack**。

要區分兩件事：

- **閘道支援這個模型** —— 我們的問題。
- **Client UI 顯示這個模型** —— Client 的問題。前者成立、後者不成立時，仍算閘道支援。

查證日期一律寫出來。文件是契約，**實測才是行為**——每個 Client 的「實測往返」欄要單獨勾。

## 總表

| Client | 狀態 | 自訂端點方式 | 協定 | 實測往返 |
| --- | --- | --- | --- | --- |
| Claude Code CLI | **supported** | `ANTHROPIC_BASE_URL` | Anthropic Messages | 2026-09-11 已通過 |
| Codex CLI | **supported** | `~/.codex/config.toml` 的 `model_providers` | OpenAI Responses | 2026-09-11 已通過 |
| Claude Desktop | **pending** | 只有 Claude apps gateway 的 `/user/bootstrap` 協定 | 該協定自有格式 | — |
| VS Code（Copilot Chat） | **supported**（聊天／agent 面） | `chatLanguageModels.json` 的 Custom Endpoint | Chat Completions／Responses／**Anthropic Messages** 三選一 | 待做 |
| Cursor | **partial**（證據等級低） | BYOK 只列五家；自訂 base URL 無官方文件 | — | — |
| Codex Desktop／IDE | **unsupported**（證據等級低） | 無官方文件 | — | — |
| 自建 Agent／自動化 Agent | **supported** | 自己設 base URL | 任一支援的協定 | 待做 |
| 本機 LLM 前端 | **supported** | OpenAI 相容 base URL | Chat Completions | 待做 |

---

## Claude Code CLI

| 欄位 | 值 |
| --- | --- |
| client | Claude Code CLI |
| version | 2.1.267（本機 `claude --version`） |
| verification_date | 2026-09-11 |
| custom_endpoint | `ANTHROPIC_BASE_URL`（shell 環境變數、`settings.json` 的 `env` 區塊、或 managed settings） |
| protocol | Anthropic Messages。端點 `POST /v1/messages`、`POST /v1/messages/count_tokens`（可選）。推論實際打 `/v1/messages?beta=true`，**比對 path 不要比對整個 URL** |
| authentication | `ANTHROPIC_AUTH_TOKEN` → `Authorization: Bearer <值>`；或 `ANTHROPIC_API_KEY` → `x-api-key`。兩個都給時兩個標頭都會送 |
| streaming | **必須**。「if your gateway buffers complete responses before relaying them, Claude Code stalls」。且必須轉送 SSE `ping` 與註解行——靜默 300 秒 Client 會中止串流 |
| tools | 支援。工具相關的 beta 欄位（`strict`、`defer_loading`）與其 beta 標頭**成對**，拆開轉送會噴 400 |
| reasoning | 支援。Claude 4.6 以後會送 `thinking: {"type":"adaptive"}`，**沒有對應的 beta 標頭**；上游模型不吃就會 400 |
| seat_subscription_behavior | 給了閘道憑證 → 訂閱**不被使用**，流量按 token 計費給憑證擁有者。只設 base URL 不給憑證 → 仍用開發者的 claude.ai 登入計費，且閘道**必須**轉送 `anthropic-beta` 裡的 OAuth capability，否則 401 |
| known_limitations | ① Anthropic 明文不支援透過任何閘道把 Claude Code 導到非 Claude 模型。② fast mode 與 WebFetch 網域檢查直接打 `api.anthropic.com`，不走 `ANTHROPIC_BASE_URL`——封鎖對外出口時 fast mode 會報連線錯誤但推論正常。③ 指向非第一方主機時 MCP tool search 預設關閉（要開得設 `ENABLE_TOOL_SEARCH=true` 且閘道會轉送 `tool_reference` 區塊）。④ v2.1.196 起指向非 `api.anthropic.com` 時 Remote Control 停用 |
| model discovery | `GET /v1/models?limit=1000`，3 秒逾時，**任何重導向都算失敗**（連 http→https 也算）。回 `data[]`，讀 `id`／`display_name`／`description`。**只保留 `id` 含 `claude` 或 `anthropic` 的項目**（不分大小寫；`vertex_ai/claude-...` 這種前綴也通得過）。Client 端要設 `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`，預設關閉 |
| 送上門的歸屬標頭 | `x-claude-code-session-id`（session 關聯，不必解析 body）、`x-claude-code-agent-id`（子 Agent）、`x-claude-code-parent-agent-id`（巢狀）。**agent id 不是人的識別**，官方明文 |
| official_source | <https://code.claude.com/docs/en/llm-gateway-protocol>、<https://code.claude.com/docs/en/llm-gateway>、<https://code.claude.com/docs/en/env-vars> |

**實測往返**：Claude Code 2.1.267 以 `ANTHROPIC_BASE_URL` 與
`ANTHROPIC_AUTH_TOKEN` 指向本機 gateway，gateway 再連可控 Anthropic SSE mock；
`claude -p --model claude-test --max-turns 1` 最終輸出精確為
`W05_CLAUDE_CODE_OK`。這證明 CLI 的 Messages 呼叫與 SSE 解析能走自架端點；
不證明真 Anthropic 帳號、正式模型或計費已被實際使用。

**設定範例**

```jsonc
// ~/.claude/settings.json
{
  "env": {
    "ANTHROPIC_BASE_URL": "https://aigw.internal.example.com",
    "ANTHROPIC_AUTH_TOKEN": "aigw_sk_...",
    "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY": "1"
  }
}
```

**閘道側必須原樣轉送的東西**（改了就壞，且有些壞法沒有錯誤訊息）：

| 項目 | 改掉的後果 |
| --- | --- |
| `anthropic-beta`、`anthropic-version` 標頭 | 做白名單 → 下一個版本的新能力被剝掉就壞掉 |
| 上游錯誤回應本體 | Client 的自動降級靠比對錯誤字串；包一層自己的 envelope 會斷掉復原路徑 |
| `system` 陣列（attribution block 保持第一個、獨立區塊） | prompt cache key 改變，**不報錯，只是每輪都算未快取** |
| `cache_control` 標記 | 同上：沒有錯誤，只是變貴 |
| SSE `ping` 事件與註解行 | 長思考時被 300 秒靜默看門狗中止 |

## Codex CLI

| 欄位 | 值 |
| --- | --- |
| client | Codex CLI |
| version | codex-cli 0.153.0（本機 `codex --version`） |
| verification_date | 2026-09-11 |
| custom_endpoint | `~/.codex/config.toml`：`model_provider = "<id>"` ＋ `[model_providers.<id>]`。內建 id `openai`／`ollama`／`lmstudio` 保留，不能覆寫 |
| protocol | **OpenAI Responses。`wire_api` 只支援 `responses`，也是省略時的預設值。** `/v1/chat/completions` 對 Codex 沒有用 |
| authentication | `env_key`（環境變數帶 key）；或 `auth`（指令產生 bearer token，可設 `refresh_interval_ms`）。另有 `experimental_bearer_token`（官方標為不建議） |
| streaming | 支援。可調 `stream_idle_timeout_ms`、`stream_max_retries` |
| tools / reasoning | 實測請求帶 17 個 tool 定義、`reasoning.effort=low` 與 `include=["reasoning.encrypted_content"]`；閘道原樣轉送 body |
| seat_subscription_behavior | `未查證`（OpenAI 側的官方原文待補） |
| known_limitations | ① 沒有官方的閘道相容契約。② Codex 0.153.0 啟動時會讀 `/v1/models` 的私有 `models[]` metadata；一般 OpenAI `data[]` 會觸發 fallback warning，但明確指定的模型仍可完成請求。完整模型能力 metadata 由 W10 註冊表提供，W06 不填假的 context window／reasoning 能力。③ 實測未送 session header；目前只能用 gateway request id 與 body 的 client metadata 關聯 |
| official_source | <https://learn.chatgpt.com/docs/config-file/config-reference> |

**設定範例**

```toml
# ~/.codex/config.toml
model_provider = "aigw"

[model_providers.aigw]
base_url = "https://aigw.internal.example.com/v1"
env_key  = "AIGW_API_KEY"
wire_api = "responses"
```

其它可用欄位：`http_headers`、`env_http_headers`、`query_params`、`requires_openai_auth`。

**實測往返**：Codex CLI 0.153.0 以自訂 `model_provider` 指向本機 gateway，gateway 再連
可控 Responses SSE mock；`codex exec` 最終輸出精確為 `W06_CODEX_OK`。capture 證明它先取
`GET /v1/models`，再送 `POST /v1/responses`；推論請求為 `stream=true`、`store=false`、
`input` 陣列、17 個工具，並帶 `originator: codex_exec`。gateway client key 沒送到 provider，
provider 只收到 server-side bearer key。這證明 CLI 的 Responses 呼叫與 SSE 解析能走自架端點；
不證明真 OpenAI 帳號、正式模型、訂閱或 API 計費已被使用。

## Claude Desktop

| 欄位 | 值 |
| --- | --- |
| 狀態 | **pending**（有官方路徑，但不是 `ANTHROPIC_BASE_URL`） |
| verification_date | 2026-09-08 |
| custom_endpoint | 走 Claude apps gateway 的 `/user/bootstrap` 設定端點，且該 gateway 的 policy 要開 `desktop` 鍵。開了之後 Cowork／Code 分頁會走閘道，Chat 分頁要另外啟用 |
| 我們要做才支援的事 | 實作 Claude apps gateway 的協定（規格由跑起來的 gateway 在 `GET /protocol` 提供） |
| 風險 | 官方明文「Breaking changes to the protocol are announced in advance, but indefinite backwards compatibility isn't guaranteed」 |
| 處置 | 列 pending。要支援得先跑一個 Claude apps gateway 抓 `GET /protocol`，成本與維護風險另評。**不阻塞其它段落** |
| official_source | <https://code.claude.com/docs/en/claude-apps-gateway> |

## VS Code（Copilot Chat）

| 欄位 | 值 |
| --- | --- |
| 狀態 | **supported**（聊天與 agent 面；程式碼補全不適用） |
| verification_date | 2026-09-08（官方文件原文，自行覆核） |
| custom_endpoint | `chatLanguageModels.json` 的 **Custom Endpoint** provider。UI：Chat: Manage Language Models → Add Models → Custom Endpoint |
| protocol | **三選一，可按 provider 或按 model 指定**：`"chat-completions"`（預設）／`"responses"`／`"messages"` |
| authentication | 設定檔的 `apiKey` |
| 設定欄位 | provider 層：`vendor`／`apiKey`／`apiType`／`url`；model 層：`id`／`name`／`url`／`toolCalling`／`vision`／`maxInputTokens`／`maxOutputTokens` |
| seat_subscription_behavior | 「BYOK models work without signing into a GitHub account and without a Copilot plan.」→ **不吃 Copilot 方案**，計費落在我們的 Provider 憑證 |
| known_limitations | 只作用在聊天與工具類任務。官方原文：「Some features still require a GitHub account: semantic search, inline suggestions (code completions), and features that rely on embeddings.」另外組織管理者可以在 GitHub.com 的 Copilot 政策關掉「Bring Your Own Language Model Key in VS Code」 |
| official_source | <https://code.visualstudio.com/docs/agent-customization/language-models> |

**這件事的份量**：VS Code 可以**直接講 Anthropic Messages**。也就是 W05 做完，
Claude Code 與 VS Code 兩個 Client 同時就通了，不必等 W06。
**證明不了**：實際往返（未實測），以及 `apiType` 是否只有這三個值——
官方頁面列了三個，我沒看到原始 JSON schema。

## Cursor

| 欄位 | 值 |
| --- | --- |
| 狀態 | **partial**，而且是架構上最弱的一個 |
| verification_date | 2026-09-08 |
| 證據等級 | **低——子 Agent 轉述，我未自行覆核官方頁面。引用前請先驗。** |
| BYOK | 官方只列五家：OpenAI／Anthropic／Google／Azure OpenAI／AWS Bedrock。設定在 Cursor Settings > Models |
| 自訂 base URL | **查不到官方來源**。有社群提到的「Override OpenAI Base URL」，但沒有官方文件 → 記 `unknown`，不當成支援 |
| known_limitations | ①「Custom API keys only work with chat models. Tab completion continues using Cursor's built-in models.」② 請求「routed through Cursor's servers for final prompt building」 |
| **對我們的真正影響** | 第 ② 點是關鍵：如果請求要經過 Cursor 的伺服器，那我們的閘道就得**從 Cursor 的機房連得到**——等於要對外曝露。這跟「Docker port 綁 `127.0.0.1`」「閘道放私有網段」直接衝突。**在解掉這點以前，Cursor 不列入支援。** |
| official_source | <https://cursor.com/help/models-and-usage/api-keys>（未自行覆核） |

## Codex Desktop／IDE 擴充

| 欄位 | 值 |
| --- | --- |
| 狀態 | **unsupported** |
| verification_date | 2026-09-08 |
| 證據等級 | **低**。依據是 vendor repo 的 issue，不是官方文件：`openai/codex#15364`「Support custom provider in app」被 OpenAI 成員於 2026-05-10 以「upvote 不足」關閉；另有 #4558／#6963／#14133 回報「擴充／Desktop 忽略 `config.toml`」仍開著 |
| 邊界 | **issue 關閉是「駁回一個請求」，不是「描述現行行為」**；issue 內文是使用者回報，不是官方聲明。「查不到文件」比「文件明文否認」弱。也未在本機實測 |
| 處置 | 記 unsupported，不阻塞。Codex **CLI** 可用，那條路已經通 |

**通則：不要因為某個 Client 的 model picker 不顯示自訂模型就寫「閘道不支援」** —— 那是 UI 的事。

## 自建 Agent、自動化 Agent、本機 LLM 前端

**supported**。自己控制程式碼的一律可以設 base URL 與金鑰，選任一支援的協定即可。
這也是最容易先跑通的實測對象，建議 W05 用它做第一次端到端驗證。
