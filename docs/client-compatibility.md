# Client 相容性

相容性只建立在官方設定介面上。明確不做 TLS 中間人、憑證注入、DNS 欺騙、破解
Client、修改執行檔或攔截訂閱憑證。查不到官方證據就寫 `unknown`，不從 issue 或社群
回報推導產品契約。

必須分開兩件事：

- **閘道支援模型**：閘道能接受該協定與模型名稱。
- **Client 顯示模型**：Client 是否把模型放進 picker。前者成立不代表後者成立。

查證日期：2026-09-11。文件證明支援邊界；「實測往返」才證明指定版本真的跑過。

## 結論

| Client | 狀態 | 自訂端點 | 協定 | 實測往返 |
| --- | --- | --- | --- | --- |
| Claude Code CLI | supported | `ANTHROPIC_BASE_URL` | Anthropic Messages | 2.1.267 已通過 |
| Codex CLI | supported | `model_providers` | OpenAI Responses | 0.153.0 已通過 |
| Claude Desktop | pending | Claude apps gateway `/user/bootstrap` | apps gateway 私有協定 | 未做 |
| VS Code Copilot Chat | supported | Custom Endpoint | Chat/Responses/Messages | 未安裝，未做 |
| Cursor | partial | BYOK／OpenAI-compatible base URL override | exact wire unknown | 未安裝，未做 |
| Codex Desktop／IDE | pending | Bedrock 模式讀共用 `~/.codex/config.toml` | self-hosted wire 未驗 | 未安裝，未做 |

## Claude Code CLI

| 欄位 | 值 |
| --- | --- |
| client | Claude Code CLI |
| version | 2.1.267（本機） |
| verification_date | 2026-09-11 |
| custom_endpoint | `ANTHROPIC_BASE_URL`，可由 shell、`settings.json` 的 `env` 或 managed settings 設定 |
| protocol | Anthropic Messages：`POST /v1/messages`；`POST /v1/messages/count_tokens` 可選 |
| authentication | `ANTHROPIC_AUTH_TOKEN` 產生 bearer；`ANTHROPIC_API_KEY` 產生 `x-api-key` |
| streaming | 必須增量轉送；SSE `ping`／comment 也算活動，預設靜默 300 秒會中止 |
| tools | supported；工具 beta body 與 `anthropic-beta` 必須原樣成對轉送 |
| reasoning | supported；Claude 4.6+ 可送 `thinking: {"type":"adaptive"}` |
| seat_subscription_behavior | 正式環境以 Gateway 金鑰認證 Client，由容器持有的訂閱權杖送出請求；多人各用自己的權杖尚未實作。舊的 Client 自帶權杖模式仍有程式支援，但正式環境未啟用。見 `architecture.md`。 |
| configuration_example | 見下方 `settings.json` |
| known_limitations | 官方不支援經 gateway 導向非 Claude 模型；fast mode／WebFetch 網域檢查可能直連 `api.anthropic.com`；非第一方端點預設停用 MCP tool search 與 Remote Control |
| official_source | <https://code.claude.com/docs/en/llm-gateway-protocol>、<https://code.claude.com/docs/en/llm-gateway>、<https://code.claude.com/docs/en/env-vars> |

```jsonc
{
  "env": {
    "ANTHROPIC_BASE_URL": "https://aigw.internal.example.com",
    "ANTHROPIC_AUTH_TOKEN": "aigw_...",
    "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY": "1"
  }
}
```

**實測證據**：2.1.267 指向本機 gateway，再到可控 Anthropic SSE mock；單次
`claude -p --model claude-test --max-turns 1` 精確輸出 `W05_CLAUDE_CODE_OK`。這證明
Messages 呼叫與 SSE 解析能走自架端點；不證明真 Anthropic 帳號、正式模型或計費。

**Gateway contract**：推論可打 `/v1/messages?beta=true`，所以比對 path 而非完整 URL；
`anthropic-beta`／`anthropic-version`、錯誤 body、`system` 陣列、`cache_control` 與 SSE raw
bytes 都不得改寫；請求只允許 router 改寫頂層 `model`。模型發現為
`GET /v1/models?limit=1000`，3 秒 timeout 且不接受 redirect；
只顯示 id 含 `claude` 或 `anthropic` 的模型。`x-claude-code-agent-id` 是 agent，不是人。

## Codex CLI

| 欄位 | 值 |
| --- | --- |
| client | Codex CLI |
| version | 0.153.0（本機） |
| verification_date | 2026-09-11 |
| custom_endpoint | `~/.codex/config.toml` 的 `model_provider` 與 `[model_providers.<id>]` |
| protocol | OpenAI Responses；`wire_api` 唯一支援值為 `responses` |
| authentication | gateway 用 `env_key` 或可定期更新 bearer 的 `auth` command，且保持 `requires_openai_auth = false`；`requires_openai_auth = true` 會改用 OpenAI 登入，見下方政策 |
| streaming | supported；可設 `stream_idle_timeout_ms`、`stream_max_retries` |
| tools | request serialization verified：隔離組態送出非空 typed tools 且含 `function`；tool-call／tool-result response 往返 unknown |
| reasoning | request serialization verified：實測送出 `reasoning.effort` 與 `reasoning.encrypted_content` include；reasoning response 往返 unknown |
| seat_subscription_behavior | OpenAI 路徑以 ChatGPT 登入時使用 ChatGPT plan；自己的 OpenAI API key 使用 API pricing。自訂 gateway credential 由 gateway／上游帳戶計費；官方文件沒有說明此模式是否仍影響 Client 端的 ChatGPT plan meter |
| configuration_example | 見下方 `config.toml` |
| known_limitations | 沒有官方 gateway compatibility contract；0.153.0 對 custom provider 不會查 gateway `/v1/models`，未知 model 會使用 fallback metadata。要消除 warning，必須另以本機 `model_catalog_json` 提供已知正確的 Codex catalog，不能由 gateway 猜 reasoning levels |
| official_source | <https://learn.chatgpt.com/docs/config-file/config-reference>、<https://learn.chatgpt.com/docs/auth>、<https://help.openai.com/en/articles/20001275>、<https://help.openai.com/en/articles/9039756> |

```toml
model = "company-default"
model_provider = "aigw"
# Optional: operator-maintained catalog with verified metadata for the model.
# model_catalog_json = "/etc/codex/company-models.json"

[model_providers.aigw]
name = "Company Gateway"
base_url = "https://aigw.internal.example.com/v1"
env_key = "AIGW_API_KEY"
wire_api = "responses"
requires_openai_auth = false
```

**實測證據**：0.153.0 指向本機 gateway，再到可控 Responses SSE mock；`codex exec`
精確輸出 `W06_CODEX_OK`。版本化 capture 顯示 `POST /v1/responses` request 為
`stream=true`、`store=false`、非空 typed tools，並帶 reasoning effort／encrypted include；
sentinel 掃過完整 headers、query 與 body，證明 gateway client key 未送到 provider。
這證明 CLI 可用；不證明真 OpenAI 帳號、正式模型、訂閱或 API 計費。

可重播 harness、固定 SSE fixture 與去敏 capture 見 `docs/client-roundtrip-evidence.md`。

Codex custom provider 的認證與帳務必須分開判斷：

| Provider 認證模式 | 憑證／付款邊界 | 本專案政策 |
| --- | --- | --- |
| `env_key` 或 `auth` command，`requires_openai_auth = false` | gateway／上游契約決定；不使用 ChatGPT login token | supported |
| `requires_openai_auth = true` ＋ ChatGPT login | OpenAI 登入與 plan 路徑 | unsupported：不得把個人／席次 token relay 給 gateway |
| `requires_openai_auth = true` ＋ OpenAI API key | OpenAI API pricing | unsupported on custom gateway：改用專用 `env_key`，避免混淆 credential owner |
| 無認證 | 沒有可歸屬的付款憑證 | unsupported in production |

`requires_openai_auth` 會改變認證來源，不能由「custom endpoint」單獨推導付款方。

## Claude Desktop

| 欄位 | 值 |
| --- | --- |
| client | Claude Desktop |
| version | unknown（本機未安裝） |
| verification_date | 2026-09-11 |
| custom_endpoint | 不使用 `ANTHROPIC_BASE_URL`；由 Claude apps gateway 的 `/user/bootstrap` 提供設定，policy 需開 `desktop` |
| protocol | Claude apps gateway protocol；規格由該 gateway 的 `GET /protocol` 提供 |
| authentication | corporate IdP/OIDC 與 gateway short-lived token；自行實作所需 wire details unknown |
| streaming | unknown（未取得 `/protocol` 規格） |
| tools | Cowork／Code 可走 gateway；Chat 要另外 opt in；逐項工具契約 unknown |
| reasoning | unknown |
| seat_subscription_behavior | 官方 apps gateway 文件描述企業 IdP 與 provider credential；Desktop seat 與 provider 費用的逐項關係 unknown |
| configuration_example | gateway policy 的 `desktop` 鍵；完整範例以官方 apps gateway 文件為準 |
| known_limitations | 官方不保證 protocol 永久向後相容；本專案尚未部署 apps gateway 或取得 `/protocol` |
| official_source | <https://code.claude.com/docs/en/claude-apps-gateway> |

**證據邊界**：官方證明 Desktop 有 gateway 路徑，也證明不是單靠 Messages base URL；不證明
第三方能穩定重作協定。因此狀態是 pending，不阻塞 CLI 路徑。

## VS Code Copilot Chat

| 欄位 | 值 |
| --- | --- |
| client | Visual Studio Code Copilot Chat／Agents |
| version | unknown（本機未安裝；rolling official docs） |
| verification_date | 2026-09-11 |
| custom_endpoint | Language Models editor 的 Custom Endpoint；設定存於 `chatLanguageModels.json` |
| protocol | provider/model 可選 `chat-completions`、`responses`、`messages` |
| authentication | `apiKey`；可用 `requestHeaders` 與 `${apiKey}` 改成 gateway 自訂 header |
| streaming | supported；model property `streaming` |
| tools | supported when `toolCalling: true`；`editTools` 可宣告編輯工具格式 |
| reasoning | `thinking`、`supportsReasoningEffort`、`reasoningEffortFormat` 可宣告 |
| seat_subscription_behavior | BYOK model 不需 GitHub 登入或 Copilot plan；semantic search、inline completion、embedding 類功能仍需 GitHub 能力 |
| configuration_example | 見下方 `chatLanguageModels.json` |
| known_limitations | 只涵蓋 chat／agent，不涵蓋 inline completion；組織可停用 BYOK；未在本機實測 |
| official_source | <https://code.visualstudio.com/docs/agent-customization/language-models> |

```jsonc
[
  {
    "name": "Company Gateway",
    "vendor": "customendpoint",
    "apiKey": "${input:aigwKey}",
    "apiType": "messages",
    "models": [{
      "id": "${modelIdFromW10}",
      "name": "Company Model",
      "url": "https://aigw.internal.example.com/v1/messages"
    }]
  }
]
```

**證據邊界**：官方文件明列三種 API、custom endpoint 與 capability 欄位，證明設定路徑
存在；不證明本 gateway 的任一模型具有特定 token limit、tool 或 reasoning 能力。範例的模型
ID 必須由 W10 registry 代入。本機沒有 `code` binary，所以不能宣稱指定版本已成功往返。

## Cursor

| 欄位 | 值 |
| --- | --- |
| client | Cursor IDE |
| version | unknown（本機未安裝；rolling official docs） |
| verification_date | 2026-09-11 |
| custom_endpoint | 官方隱私文件承認 OpenAI-compatible base URL override／third-party gateway；精確設定 schema unknown |
| protocol | OpenAI-compatible；實際 endpoint 與 request shape unknown |
| authentication | BYOK 支援 OpenAI、Anthropic、Google、Azure OpenAI、AWS Bedrock；key 隨每次 request 經 Cursor backend，但官方稱不持久化 |
| streaming | unknown |
| tools | custom key 只適用 standard chat models；Tab 等特殊功能仍用 Cursor models |
| reasoning | OpenAI BYOK 官方限定 standard non-reasoning chat models；其它 provider 細節 unknown |
| seat_subscription_behavior | BYOK 是「at your own cost」；是否仍消耗 Cursor seat quota unknown，不能推測 |
| configuration_example | Cursor Settings > Models > provider key；base URL override 的正式 schema unknown |
| known_limitations | 所有 request 即使 BYOK 仍先到 Cursor backend 做 final prompt building；目前 `127.0.0.1`／private-only 部署預設不可達；enterprise private connectivity 是否涵蓋 custom LLM gateway unknown；BYOK 不適用 Cursor Zero Data Retention |
| official_source | <https://prod.cursor.com/help/models-and-usage/api-keys>、<https://www.cursor.com/security>、<https://prod.cursor.com/docs/enterprise/privacy-and-data-governance>、<https://prod.cursor.com/docs/cloud-agent/private-connectivity> |

**證據邊界**：官方來源證明 BYOK、base URL override 與第三方 gateway 確實存在，也明說
request 仍經 Cursor 基礎設施；不證明目前 wire protocol 可通過本 gateway。狀態因此是 partial，
而不是 supported。部署若只綁 `127.0.0.1` 或 private-only，預設不可達；Cursor 的 enterprise
private connectivity 是否能連 custom LLM gateway 沒有官方證據，維持 `unknown`。

## Codex Desktop／IDE

| 欄位 | 值 |
| --- | --- |
| client | Codex Desktop app／VS Code extension |
| version | 本機 unknown；官方 Bedrock 路徑要求 app／extension `26.429.30905` 以上 |
| verification_date | 2026-09-11 |
| custom_endpoint | 官方 Bedrock 設定明寫兩者讀 `~/.codex/config.toml` 與 `~/.codex/.env`；任意 self-hosted `model_providers` 是否等價套用 unknown |
| protocol | Bedrock provider 使用 Responses；自架 gateway 的實際 wire 未驗 |
| authentication | Bedrock 可用 bearer 或 AWS credential chain；任意自架 provider 應使用 `env_key`／`auth` 且停用 `requires_openai_auth`，實際 UI 行為 unknown |
| streaming | unknown for self-hosted provider |
| tools | Bedrock 模式已知缺 image generation、voice、cloud plugin store／agents；self-hosted 細節 unknown |
| reasoning | supported model-dependent for documented Bedrock models；self-hosted metadata behavior unknown |
| seat_subscription_behavior | OpenAI 路徑以 ChatGPT 登入時使用 plan、以 OpenAI API key 時使用 API pricing；Bedrock 由 AWS 帳戶計費；任意自架 provider 的 Client 端 plan meter 行為 unknown |
| configuration_example | 官方只證實 Bedrock；自架 gateway 暫沿用 CLI 範例但不得宣稱已支援 |
| known_limitations | 需重啟 app／extension 才重讀 config；目前沒有官方證據證明任意 custom provider 在 UI client 完整工作 |
| official_source | <https://help.openai.com/en/articles/20001253-configure-codex-with-amazon-bedrock>、<https://learn.chatgpt.com/docs/config-file/config-reference>、<https://learn.chatgpt.com/docs/auth>、<https://help.openai.com/en/articles/20001275> |

**證據邊界**：官方 Bedrock 文件推翻「Desktop／IDE 完全忽略 config」的舊結論，也給出
`26.429.30905` 的最低版本；但它只證明 Bedrock，不能外推任意 gateway，也不能證明自架
provider 的 Client 端 plan meter 行為。因此由 unsupported 改為 pending，待安裝後做一次和
CLI 相同的本機 Responses 往返。
