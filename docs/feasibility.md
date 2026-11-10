# 可行性

**問題**：這個閘道最終真的做得出來嗎？

**結論：做得出來。** 核心路徑（Claude Code、Codex CLI、**VS Code**）都可行，
而且 Claude Code 那條還有**官方明文規範的相容契約**可以照著實作，不需要任何非官方手段。

三個 Client 支援（Claude Code、Codex CLI、VS Code）、兩個 pending（Claude Desktop、
Codex Desktop／IDE 的任意自架 provider）、一個受阻於架構而非協定（Cursor）。
**都不阻塞整體架構。**

**額外收穫**：VS Code 的 Custom Endpoint 可以直接講 Anthropic Messages。
W05 已完成 gateway 端契約與 Claude Code 實測；VS Code 仍只有官方文件證據，尚未本機往返。

查證日期：**2026-09-11**。Claude Code 本機版本 `2.1.267`，Codex CLI 本機版本 `0.153.0`。

> 每條結論都標了「這證明什麼、證明不了什麼」。沒有官方來源的一律寫 `未查證`，不寫推測。

---

## 一、最重要的發現：Anthropic 官方明文支援這個用途

Anthropic 有一整組文件在講「把 Claude Code 指向你自己營運的 LLM 閘道」，
其中 **Gateway compatibility guide** 就是一份給閘道實作者看的**相容契約**。

> 「A gateway must expose at least one of the following API formats to Claude Code clients.」
> — <https://code.claude.com/docs/en/llm-gateway-protocol>（2026-09-08）

> 「A gateway gives your organization one place to manage: **Credentials** — the provider key stays
> server-side; developers hold gateway credentials instead. **Usage tracking** — attribute usage by
> developer or team... **Cost controls** — enforce budgets and rate limits in one place.
> **Audit logging** — log every model request for compliance.」
> — <https://code.claude.com/docs/en/llm-gateway>（2026-09-08）

**這證明**：本專案的目標情境（開發者持閘道憑證、Provider 憑證留在伺服器、按人歸屬用量、
集中預算與稽核）是 Anthropic 文件裡明列的預期用途，不是繞路。

**也證明了一條硬限制**：

> 「Anthropic doesn't endorse, maintain, or audit third-party gateway products, and **doesn't support
> routing Claude Code to non-Claude models through any gateway**.」
> — <https://code.claude.com/docs/en/llm-gateway>（2026-09-08）

→ 規格 §72 的「Anthropic 協定 Client 用 OpenAI 模型」在技術上做得到，但**不在 Anthropic 支援範圍**。
W15 必須把這件事寫成明確的風險，不能當成一般功能宣傳。

---

## 二、逐個 Client

### Claude Code CLI — **supported**

| 項目 | 值 |
| --- | --- |
| 版本 | 2.1.267（本機實測版本） |
| 自訂端點 | `ANTHROPIC_BASE_URL` |
| 協定 | Anthropic Messages |
| 認證 | `ANTHROPIC_AUTH_TOKEN`（自動加 `Bearer ` 前綴）或 `ANTHROPIC_API_KEY`（走 `x-api-key`） |
| 串流 | 必須。緩衝會讓 Client 卡住 |
| 設定管道 | shell 環境變數、`settings.json` 的 `env` 區塊、managed settings |

官方原文：

> `ANTHROPIC_BASE_URL` — 「Override the API endpoint to route requests through a proxy or gateway.」
> `ANTHROPIC_AUTH_TOKEN` — 「Custom value for the `Authorization` header (the value you set here
> will be prefixed with `Bearer `)」
> — <https://code.claude.com/docs/en/env-vars>（2026-09-08）

**閘道必須做到的事（這是契約，不是建議）**：

1. **端點**：`POST /v1/messages`、`POST /v1/messages/count_tokens`（可選）。
   推論請求實際打的是 **`/v1/messages?beta=true`** ——
   > 「Inference requests post to `/v1/messages?beta=true`, so match on the path, not the full URL.」
2. **不得緩衝**：
   > 「Claude Code reads the stream as it arrives, so if your gateway buffers complete responses
   > before relaying them, Claude Code stalls.」
3. **必須轉送 SSE keep-alive ping**，否則長思考期間會被判定斷線：
   > 「Claude Code counts every byte your gateway relays, including SSE `ping` events and comment
   > lines, and **aborts a stream that goes silent for 300 seconds** by default.」
4. **`anthropic-beta` 與 `anthropic-version` 標頭逐字轉送，且不得做白名單**：
   > 「Forward the header verbatim; don't allowlist individual values, because the set changes with
   > Claude Code releases.」
5. **錯誤回應本體不得改寫**：Claude Code 的自動降級靠比對上游錯誤字串。
   > 「A gateway that wraps upstream errors in its own envelope breaks the recovery path, even when
   > it preserves the status code, unless the envelope's message carries a stable
   > `capability_rejected:` token.」
6. **`system` 陣列與 `cache_control` 標記原樣轉送**，否則 prompt cache 失效（無錯誤、只是變貴）。
7. 啟動時會收到 `HEAD /api/hello` 連線預熱探測，可以直接拒絕不影響功能。

**送上門的身分資訊**（規格 §10 的 Principal 解析因此省一大半工）：

| 標頭 | 內容 |
| --- | --- |
| `x-claude-code-session-id` | 「A unique identifier for the current Claude Code session.」 |
| `x-claude-code-agent-id` | 子 Agent 的識別碼，只在 Client 自己 spawn 的 Agent 請求上出現 |
| `x-claude-code-parent-agent-id` | 巢狀 Agent 的父層識別碼 |

但**不能**拿 agent id 當人的識別：

> 「the ID identifies an agent, not a person or a device, so don't treat the agent ID header as a
> user identifier.」

**模型發現**：`GET /v1/models?limit=1000`，3 秒逾時，**不得重導向**，回 `data[]` 含
`id`／`display_name`／`description`。Client 端要設
`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` 才會查，且**只保留 `id` 含 `claude` 或
`anthropic` 的項目**（不分大小寫）。

**這證明**：閘道端完全做得到，且每個實作細節都有明文。W05 另以 Claude Code 2.1.267
指向本機 gateway 與可控 SSE mock，精確輸出 `W05_CLAUDE_CODE_OK`。
**證明不了**：真 Anthropic 帳號、正式模型或正式計費已驗證。

來源：
- <https://code.claude.com/docs/en/llm-gateway-protocol>
- <https://code.claude.com/docs/en/llm-gateway>
- <https://code.claude.com/docs/en/env-vars>

### Codex CLI — **supported（但只有一種 wire 協定）**

| 項目 | 值 |
| --- | --- |
| 版本 | codex-cli 0.153.0（本機實測版本） |
| 自訂端點 | `~/.codex/config.toml` 的 `model_providers.<id>` ＋ `model_provider = "<id>"` |
| 協定 | **`wire_api = "responses"`（唯一支援值，也是預設值）** |
| 認證 | `env_key`（環境變數帶 API key）；或 `auth`（指令產生 bearer token，可設 refresh 間隔） |
| 其它可用欄位 | `http_headers`、`env_http_headers`、`query_params`、`requires_openai_auth`、`stream_idle_timeout_ms`、`stream_max_retries` |

官方原文：

> 「Custom provider definition. Built-in provider IDs (`openai`, `ollama`, and `lmstudio`) are
> reserved and cannot be overridden.」
> 「Protocol used by the provider. **`responses` is the only supported value**, and it is the
> default when omitted.」
> — <https://learn.chatgpt.com/docs/config-file/config-reference>（2026-09-08）

**這直接改變 W06 的優先序**：Codex 只走 **OpenAI Responses API**。
`POST /v1/chat/completions` 對 Codex **沒有用**——它留給其它 OpenAI 相容 Client。
所以 W06 要先做 `/v1/responses`，`/v1/chat/completions` 排在後面。

**這證明**：Codex 可以指向自架端點，且支援用外部指令輪替 bearer token（對短效憑證是好事）。
W06 已以 Codex CLI 0.153.0 實測：送 `POST /v1/responses`，包含串流、非空 typed tools
與 reasoning 欄位，最後精確輸出 `W06_CODEX_OK`。
**證明不了**：任意版本都維持相同私有模型 metadata 契約；這個剩餘缺口已移交 W10。

### Claude Desktop — **pending：只透過 Claude apps gateway 協定有官方路徑**

Claude Desktop **不吃** `ANTHROPIC_BASE_URL`。它走的是 Claude apps gateway 的
`/user/bootstrap` 設定端點，且要對應的 policy 開 `desktop` 鍵：

> 「The gateway serves Claude Desktop's configuration at `/user/bootstrap` once a policy opts in
> with a `desktop` key, and Claude Desktop sends model requests from its Cowork and Code tabs, and
> from the Chat tab when you enable it, through the gateway.」
> — <https://code.claude.com/docs/en/claude-apps-gateway>（2026-09-08）

**這證明**：Claude Desktop 走自架閘道**有官方路徑**，但那條路徑是「實作 Claude apps gateway 的
協定」，不是「實作 Messages API」。
**證明不了**：我們能不能自己實作那個協定。它的規格由跑起來的 gateway 在 `GET /protocol` 提供，
且官方寫明「Breaking changes to the protocol are announced in advance, but indefinite backwards
compatibility isn't guaranteed」。

**處置**：列為 `pending`。要支援就得先跑一個 Claude apps gateway 抓 `GET /protocol`，成本與
維護風險都要另外評估。**不阻塞其它段落。**

### VS Code（Copilot Chat）— **supported**（聊天／agent 面）

自行覆核官方文件（2026-09-08）：Custom Endpoint provider 支援
**`"chat-completions"` / `"responses"` / `"messages"` 三種 apiType**，可按 provider 或按 model 指定。

> 「The Custom Endpoint provider lets you connect any compatible API endpoint to chat in VS Code.
> It supports three API types... Chat Completions, Responses, and the Anthropic Messages API.」
> 「BYOK models work without signing into a GitHub account and without a Copilot plan.」
> — <https://code.visualstudio.com/docs/agent-customization/language-models>

**這件事的份量**：VS Code 可以直接講 Anthropic Messages，W05 已提供相容的 gateway 端路徑；
但本機沒有 `code` binary，不能把 Claude Code 的實測算成 VS Code 已實測。
**限制**：只作用在聊天與工具類任務；程式碼補全、語意搜尋、embeddings 仍需 GitHub 帳號。
組織管理者可以在 GitHub.com 關掉 BYOK 政策。

### Cursor — **partial，而且架構上最弱**

2026-09-11 已自行覆核 Cursor 的 API key、security、privacy 官方頁。BYOK 列五家 Provider，
隱私文件也承認 OpenAI-compatible base URL override／third-party gateway；精確 wire 仍是 unknown。

**真正的問題不是協定**：官方說所有請求仍經 Cursor backend 做 final prompt building。
因此 `127.0.0.1` 或 private-only 的預設部署無法直接被 Cursor backend 連入。Cursor 另有
enterprise private connectivity，但官方目前只說明 source-control／package 類系統，沒有說它能
連 custom LLM gateway；可受支援的私連路徑維持 `unknown`。在證明以前 Cursor 不列入支援。

### Codex Desktop／IDE 擴充 — **pending for arbitrary self-hosted providers**

OpenAI 的 Bedrock 官方文件明列 Codex CLI、Desktop app、VS Code extension 三者都支援該 provider，
且 app／extension 讀 `~/.codex/config.toml` 與 `~/.codex/.env`；app／extension 最低版本為
`26.429.30905`。這推翻舊的「完全忽略 config」結論；但文件只證明 Bedrock，不能外推任意
自架 gateway。未安裝實測前維持 pending，不阻塞 CLI。

---

## 三、席次與 API 計費的邊界（規格 §7）

這條有官方明文，而且講得很死：

> 「While a gateway credential variable or `apiKeyHelper` is active, a developer's claude.ai
> subscription **isn't used**: the credential replaces the subscription login for that session, and
> the subscription's usage limits don't apply. **That traffic is billed per token to whoever owns
> the credential** the gateway forwards, such as your organization's Anthropic Console account...」
> — <https://code.claude.com/docs/en/llm-gateway>（2026-09-08）

> `ANTHROPIC_API_KEY` — 「When set, this key is used **instead of** your Claude Pro, Max, Team, or
> Enterprise subscription even if you are logged in.」
> — <https://code.claude.com/docs/en/env-vars>（2026-09-08）

還有一個容易踩的細節：

> 「Setting only `ANTHROPIC_BASE_URL`, without a gateway credential, **doesn't replace the
> subscription**. Requests still route through the gateway, but a saved claude.ai login remains the
> active credential, so its usage limits and billing apply. Gateways that pass this traffic on to
> Anthropic must forward the OAuth capability in `anthropic-beta`.」

**結論**：
1. 用閘道憑證（API 金鑰）→ 計費落在**憑證擁有者**，訂閱席次不被動用。這是可選的另一條路，不是必須。
2. 只設 base URL 不給憑證 → 流量經過閘道但**仍用開發者的訂閱**計費。
   這種模式閘道必須轉送 `anthropic-beta` 裡的 OAuth capability，**否則 401**。
   這是舊模式；正式環境現已改為 Client 帶 Gateway 金鑰、訂閱權杖由容器持有，見 `architecture.md`。多人各用自己的權杖尚未實作。
3. 我們**不做**第 2 種模式的「把席次轉成 API credit」——那從來不成立，官方規則就是分開計費。

OpenAI 側也已於 2026-09-11 覆核：官方寫明 ChatGPT 與 API 是兩套 billing system；
Codex 以 ChatGPT 登入使用 ChatGPT plan，改用自己的 OpenAI API key 則走 API pricing。
這兩份文件沒有說明任意自架 provider 是否仍影響 Client 端 ChatGPT plan meter，因此該互動標
`unknown`；gateway 只接受自己的 credential，不轉送 ChatGPT 登入 token。

Codex custom provider 還有一個獨立開關：`requires_openai_auth = true` 會改用 OpenAI
認證，而不是 `env_key`。本專案只支援 `requires_openai_auth = false` 搭配 gateway
`env_key`／`auth` command；ChatGPT login token relay、custom provider 上混用 OpenAI API key、
以及 production 無認證三種模式都不支援。

**這證明**：規格 §7 的邊界主張對兩家都成立，且有官方文字支撐。
**證明不了**：第三方 gateway 的費用是否由 Client 額外加價，或是否仍計入 Client 的方案用量；
那要看各 Client 自己的文件與方案。

OpenAI 來源：<https://help.openai.com/en/articles/20001275>、
<https://help.openai.com/en/articles/9039756>、<https://learn.chatgpt.com/docs/auth>、
<https://learn.chatgpt.com/docs/config-file/config-reference>。

---

## 四、必須先知道的重大事實：有一個第一方的同類產品

Anthropic 自己出了 **Claude apps gateway**，就在 `claude` 執行檔裡，`claude gateway --config
gateway.yaml` 就能跑。

> 「Claude apps gateway is a self-hosted service that sits between your developers' Claude Code
> clients and your model provider. Developers sign in with your corporate identity provider (IdP)
> instead of holding API keys or cloud credentials. The gateway holds the upstream credential,
> enforces model access and managed settings by IdP group, and relays usage telemetry to your own
> observability stack. **It is included in the `claude` binary.**」
> — <https://code.claude.com/docs/en/claude-apps-gateway>（2026-09-08）

它**已經有**的（我們規格裡也有的）：

| 能力 | 官方狀態 |
| --- | --- |
| 上游憑證留伺服器、開發者走 SSO 拿短效 token | Available（預設 1 小時） |
| 依 IdP 群組的模型白名單（伺服器端強制） | Available |
| OTLP/HTTP 遙測（token 數、模型、使用者身分、延遲） | Available |
| **每人／每群組的支出上限** | Available |
| Claude Desktop | Available（需 policy opt-in） |
| 多上游（Bedrock／AWS／GCP／Foundry／Anthropic API）＋ failover | Available |

它**沒有**的：

| 缺口 | 官方狀態 |
| --- | --- |
| **Admin UI** | 「Not available — Configuration is the YAML file; redeploy to change it」 |
| OTLP/gRPC | Not supported |
| 非 OIDC 認證（SAML／LDAP） | Not supported |
| 多租戶（多個 OIDC issuer） | Not supported |
| Windows server | Not supported |
| 伺服器端 web search | Not available |
| 1 小時 prompt cache TTL | Not available（閘道 session 一律 5 分鐘） |

以及（文件通篇未提，`未查證`＝視為不具備）：
**OpenAI／Gemini／xAI／本機模型**、**跨協定轉換**、
**請求排程／加權公平佇列／保留容量**、**可自由查詢的 PostgreSQL 用量帳本**、
**VS Code／Cursor／Codex 這些非 Claude Client**。

### 這對本專案的意思

**本專案的差異化就是那份缺口清單**，尤其是四項：
① 多 Provider ② 排程與保留容量 ③ 可查詢的精確用量帳本 ④ Admin UI。

如果需求其實只有「Claude Code ＋ SSO ＋ 支出上限 ＋ 遙測」，
**那第一方產品已經做完了，這 18 段落大部分是重工**。

**建議**：繼續做，但把「Claude apps gateway 已覆蓋的部分」當成**可替換的下層**看待——
不要在 SSO、managed settings 這種它已經做好且會持續跟版的地方投入。
本專案的價值放在它明確沒有的四項上。

**這是需要人決定的事，不是我能自己決定的。** 已列進 PR #21 的討論。

---

## 五、其它技術可行性

| 項目 | 結論 | 依據 |
| --- | --- | --- |
| Rust ＋ Axum ＋ Tokio 做 SSE 增量代理 | 可行 | 既有 `src/services/chat.rs` 已能轉發；增量轉送是 Axum/Tokio 的常規用法 |
| 取消傳遞（Client 斷線 → 取消上游） | 可行，但**要實測** | Tokio 的 task cancellation ＋ reqwest 的 drop 會關連線；「Provider 是否停止計費」`未查證`，Provider 文件沒有明文承諾 |
| PostgreSQL 當精確用量帳本 | 可行 | 一般用法，無技術風險 |
| OTLP 三路（trace／metric／log） | 可行 | `opentelemetry` crate 生態成熟 |
| 加權公平佇列 ＋ aging ＋ 保留容量 | 可行 | 純內部演算法，不依賴外部行為 |

**取消計費那條要特別講清楚**：規格 §39 要求「Client 已離開就不要繼續計費」。
閘道能做的是**關掉上游連線**；上游收到中斷後會不會停止生成與計費，是 Provider 的行為，
**目前沒有官方明文**。所以 W05 的完成判準寫的是「mock provider 端必須觀察到連線被關閉」，
而不是「Provider 停止計費」——後者我們證明不了，就不宣稱。

---

## 六、Blocker 盤點

| # | 項目 | 是 blocker 嗎 |
| --- | --- | --- |
| 1 | Claude Code 走自架閘道 | **不是**。官方明文支援，且有實作契約 |
| 2 | Codex CLI 走自架閘道 | **不是**。官方支援，但只有 `responses` 一種 wire 協定 |
| 3 | Claude Desktop | **不是**（不阻塞）。要走 Claude apps gateway 協定，列 pending |
| 4 | VS Code | **不是**。官方支援 Custom Endpoint，且可講 Anthropic Messages |
| 4b | Cursor | **不是**（不阻塞）。目前 private-only 部署預設不可達；enterprise private connectivity 是否涵蓋 custom LLM gateway 為 unknown |
| 4c | Codex Desktop／IDE | **不是**（不阻塞）。Bedrock 有官方支援；任意自架 provider 待實測，記 pending |
| 5 | 跨 Provider 給 Claude Code 用非 Claude 模型 | **不是 blocker，但是風險**。Anthropic 明文不支援，W15 要標註 |
| 6 | 訂閱席次轉 API credit | **不做**。官方規則就是分開計費，本專案從一開始就不碰 |
| 7 | 取消後上游是否停止計費 | **不是 blocker**。閘道盡力關連線，不宣稱 Provider 行為 |
| 8 | 與第一方 Claude apps gateway 重疊 | **不是技術 blocker，是範圍決策**。見第四節 |

**沒有技術 blocker。可以往下做。**
