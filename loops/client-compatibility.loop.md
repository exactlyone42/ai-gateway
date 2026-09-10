# LOOP: client-compatibility

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#18](https://github.com/DnaBoss/agent-pool-gateway/issues/18) [#15](https://github.com/DnaBoss/agent-pool-gateway/issues/15) [#5](https://github.com/DnaBoss/agent-pool-gateway/issues/5) [#7](https://github.com/DnaBoss/agent-pool-gateway/issues/7)｜分支 `feature/client-compatibility`｜段落 `W01`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P0 |
| 前置 | 無 |
| 開始 | `2026-09-08T21:50+08:00` |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
回答「哪些 Client 真的能指向自架閘道」，以及「訂閱席次與 API 計費的邊界在哪」，
每條結論都附得出官方來源。

## 完成判準
- [x] `docs/client-compatibility.md` 每個 Client 一條，欄位齊全：
      client／version／verification_date／custom_endpoint／protocol／authentication／streaming／
      tools／reasoning／seat_subscription_behavior／configuration_example／known_limitations／official_source。
- [x] 每條結論標明「這證明什麼、證明不了什麼」。
- [x] 查不到官方來源的一律標 `unknown`，**不寫推測**。
- [x] 明確區分「閘道支援這個模型」與「Client UI 顯示這個模型」。
- [x] 訂閱席次邊界寫進 `docs/security-and-compliance.md`，附官方定價／方案頁原文。
- [x] **實測**：至少一個 Client 真的指向本機閘道並成功往返一次（不是只讀文件）。
- [x] **陰性對照**：移除任一 Client 的必要欄位，`./scripts/check.sh clients` 必須變紅。

## 已知
- 待查的 Client：Claude Code CLI、Claude Desktop、Codex CLI、Codex Desktop／IDE、
  VS Code（Copilot Chat）、Cursor。
- 既有 `docs/client-compatibility.md` 只有 17 行，是 LiteLLM 時期的粗略描述，沒有版本、
  沒有查證日期、沒有官方來源 → 不能當證據，要重寫。
- 硬約束（規格 §6）：不做 TLS 中間人、不注入憑證、不破解 Client、不改執行檔、
  不攔截訂閱憑證。某個 Client 不支援就標 unsupported，不硬 hack。
- 2026-09-11 本機只有 Claude Code 2.1.267 與 Codex CLI 0.153.0；沒有 `code`／`cursor`
  binary，所以 VS Code、Cursor、Codex Desktop／IDE 的行為只標官方文件等級，不冒充本機實測。
- Cursor 官方來源已自行覆核：BYOK／base URL override 存在，但所有 request 仍經 Cursor backend；
  `127.0.0.1`／private-only 預設不可達，enterprise private connectivity 是否涵蓋 custom LLM
  gateway 為 unknown。Codex 官方 Bedrock 文件證明 Desktop／VS Code extension 讀共用 config，
  但不能外推任意 self-hosted provider，狀態改為 pending。

## 已做
- `2026-09-08` 起 `compat-research` 子 Agent 查證 9 組問題（各 Client 自訂端點、席次邊界、
  SSE 事件形狀、取消語意、`count_tokens` 是否存在）。要求輸出官方 URL ＋ 逐字引用 ＋ 邊界。
- `2026-09-08` 主 session 自行覆核最關鍵的四條（不採信轉述）：Claude Code 自訂端點、
  Codex 自訂 provider、席次邊界、Claude Desktop。結論與原文引用寫進 `docs/feasibility.md`。
- `2026-09-08` 查到 Anthropic 有**整組官方閘道文件**，其中一份是給閘道實作者的相容契約。
  這是本段落最大的收穫：不必逆推行為，照契約寫即可。
- `2026-09-08` 查到第一方同類產品 Claude apps gateway（在 `claude` 執行檔內）。
  能力與缺口對照寫進 `docs/feasibility.md` 第四節，屬於**要人決定的範圍問題**。
- `2026-09-08` 自行覆核 VS Code：Custom Endpoint 支援 `chat-completions`／`responses`／
  **`messages`** 三種 apiType，且不需要 Copilot 方案。→ VS Code 升為 supported。
- `2026-09-08` 收子 Agent 對 Cursor 與 Codex Desktop 的查證，兩者證據等級都低（Cursor 是轉述、
  Codex Desktop 只有 issue 不是文件），已在文件標明證據等級，不當成已驗證。
- `2026-09-11` Claude Code 2.1.267 以 `ANTHROPIC_BASE_URL`／`ANTHROPIC_AUTH_TOKEN`
  指向本機 gateway，再到可控 Anthropic SSE mock；單次 `claude -p` 精確輸出
  `W05_CLAUDE_CODE_OK`。這封閉至少一個真 Client 往返判準，但 W01 的研究缺口仍未完成。
- `2026-09-11` Codex CLI 0.153.0 經本機 gateway 與可控 Responses SSE mock，精確輸出
  `W06_CODEX_OK`；版本化 capture 鎖定 POST responses、stream/store、typed tools、reasoning
  與完整 request sentinel 掃描；隔離組態沒有先查 models，不沿用舊環境的順序主張。
- `2026-09-11` 重寫六個 Client 的統一 13 欄契約，補齊 Cursor、Codex UI client、VS Code
  與 OpenAI billing 官方證據；席次／API 邊界同步進 security 文件。
- `2026-09-11` 新增 client compatibility 文件關卡，納入 `gate`／`all`。
- `2026-09-11` 第一輪反方審查指出 2 high／4 medium；採納全部事實缺口：拆開 Codex 四種
  認證模式、收斂 Cursor 私連結論、移除 VS Code 虛構能力、限定 tools／reasoning 證據範圍，
  並把 `clients` gate 明定為結構檢查，不拿它代替來源人工覆核。
- `2026-09-11` 補 `scripts/verify-client-roundtrip.sh`、本機 mock、兩份 SSE fixture 與版本化去敏
  capture；Claude Code 2.1.267、Codex CLI 0.153.0 各重播成功。第一次重播另抓到 Codex
  custom provider 缺 `name` 會在送 request 前拒絕設定，已同步修文件範例。
- `2026-09-11` 完整 diff review 指出 1 high／4 medium，全部採納：runtime 改 `mktemp` 且 DB
  要顯式 mutation flag＋測試庫名稱；port 改動態且驗子程序 PID；capture 掃完整 request sentinel
  並鎖定 stream／store／tool types／reasoning；OpenAI API key 標成 gateway 外；Cursor 移除
  「等於公開」絕對句。精確 capture 也推翻舊的 GET models／全 function tools 敘述，已刪除。
- `2026-09-11` PR 級檢查：69 lib＋2 schema＝71 passed、0 failed、0 ignored；29/29 gate
  selftests；client capture schema、log secrets、loops、bans、deadfiles 全綠。

## 已確定不可行
- **Claude Desktop 用 `ANTHROPIC_BASE_URL`**：Desktop 走的是 Claude apps gateway 的
  `/user/bootstrap` 設定端點 ＋ policy 的 `desktop` 鍵，不是環境變數。
  要支援得實作那個協定（規格由跑起來的 gateway 在 `GET /protocol` 提供，且官方寫明
  不保證無限期向後相容）。列 pending，不阻塞其它段落。
- **把訂閱席次轉成 API credit**：官方規則就是分開計費，這條從一開始就不成立，不再花時間查。
- **Cursor 直接走 `127.0.0.1`／private-only gateway**：官方說所有請求經 Cursor backend 做
  final prompt building，因此預設不可達。enterprise private connectivity 是否支援 custom LLM
  gateway 沒有官方證據，標 `unknown`，不宣稱架構上永遠不可能。

## 證據
- `ANTHROPIC_BASE_URL`「Override the API endpoint to route requests through a proxy or gateway.」
  → **這證明** Claude Code 官方支援指向自架閘道；W05 已另以 2.1.267 完成本機往返。
  **證明不了**真 Anthropic 帳號、正式模型或計費已驗證。
- `wire_api`「`responses` is the only supported value, and it is the default when omitted.」
  → **這證明** Codex 只走 Responses API；W06 已另以 0.153.0 實測實際欄位。
  **證明不了**未來版本仍維持相同私有模型 metadata 契約，這個缺口移交 W10。
- 「While a gateway credential variable or `apiKeyHelper` is active, a developer's claude.ai
  subscription isn't used... billed per token to whoever owns the credential.」
  → **這證明** Anthropic 席次與 API 計費的邊界。OpenAI 另有官方文件明寫 ChatGPT 與 API
  billing 分離、Codex ChatGPT login 用 plan、自有 API key 用 API pricing。
- 本機版本：Claude Code `2.1.267`、codex-cli `0.153.0`（`claude --version` / `codex --version`）。
  → 兩者都完成真 Client 往返；證明不了真 Provider 帳號、正式模型與計費。
- `./scripts/check.sh clients`：六個 Client 各 13 欄、官方 URL 與證據邊界全通過。刪除 Cursor
  `streaming` 欄時精準 exit 1，恢復後通過。這只是結構 gate；來源與逐欄主張另由人工對照，
  gate 證明不了 URL 內容、結論正確性或官方頁未來不會改版。
- `CLIENT_ROUNDTRIP_ALLOW_DATABASE_MUTATION=1 DATABASE_URL=<disposable> ./scripts/verify-client-roundtrip.sh all`：
  兩個真 Client 各一次往返；固定命令、fixture、去敏
  capture 與證據邊界見 `docs/client-roundtrip-evidence.md`。
- 去敏 gate 陰性對照：暫時加入 `authorization` 欄位後，只有 `clients` 以
  `unapproved fields: ['authorization']` 變紅；還原後通過。
- harness 安全陰性對照：缺 mutation flag 與帶 `production` DB 名稱各自 exit 2，均在啟動
  mock／gateway 前停止；證明文件提醒已變成程式守門，不會誤連正式 DB。

## 來源
- <https://code.claude.com/docs/en/llm-gateway-protocol>（閘道相容契約）2026-09-08
- <https://code.claude.com/docs/en/llm-gateway>（席次邊界、不支援非 Claude 模型）2026-09-08
- <https://code.claude.com/docs/en/env-vars>（環境變數逐條說明）2026-09-08
- <https://code.claude.com/docs/en/claude-apps-gateway>（第一方閘道能力與限制表）2026-09-08
- <https://learn.chatgpt.com/docs/config-file/config-reference>（Codex `model_providers`）2026-09-11
- <https://help.openai.com/en/articles/20001275>（Codex plan／API key 計費）2026-09-11
- <https://help.openai.com/en/articles/9039756>（ChatGPT／API 分開計費）2026-09-11
- <https://help.openai.com/en/articles/20001253-configure-codex-with-amazon-bedrock>（UI clients）2026-09-11
- <https://code.visualstudio.com/docs/agent-customization/language-models>（VS Code Custom Endpoint）2026-09-11
- <https://prod.cursor.com/help/models-and-usage/api-keys>、<https://www.cursor.com/security>（Cursor）2026-09-11
- <https://prod.cursor.com/docs/cloud-agent/private-connectivity>（Cursor enterprise 私連邊界）2026-09-11
- <https://learn.chatgpt.com/docs/auth>（Codex 認證模式）2026-09-11

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 子 Agent 查證 | 子 Agent 廣搜 → 主 session 覆核關鍵 URL | 主 session 自己全查 | 全查會把數十頁文件讀進主 context；但「其它 Agent 的報告」不算證據，關鍵條目要自己覆核 |

## 下一步
1. 提交、PR、合 `dev`，確認 CI。
2. Claude apps gateway 採用與否仍是產品範圍決策，不阻塞 W01；Codex UI client 實測可另開卡。
