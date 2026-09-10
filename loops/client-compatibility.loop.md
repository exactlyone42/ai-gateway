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
- [ ] `docs/client-compatibility.md` 每個 Client 一條，欄位齊全：
      client／version／verification_date／custom_endpoint／protocol／authentication／streaming／
      tools／reasoning／seat_subscription_behavior／configuration_example／known_limitations／official_source。
- [ ] 每條結論標明「這證明什麼、證明不了什麼」。
- [ ] 查不到官方來源的一律標 `unknown`，**不寫推測**。
- [ ] 明確區分「閘道支援這個模型」與「Client UI 顯示這個模型」。
- [ ] 訂閱席次邊界寫進 `docs/security-and-compliance.md`，附官方定價／方案頁原文。
- [ ] **實測**：至少一個 Client 真的指向本機閘道並成功往返一次（不是只讀文件）。

## 已知
- 待查的 Client：Claude Code CLI、Claude Desktop、Codex CLI、Codex Desktop／IDE、
  VS Code（Copilot Chat）、Cursor。
- 既有 `docs/client-compatibility.md` 只有 17 行，是 LiteLLM 時期的粗略描述，沒有版本、
  沒有查證日期、沒有官方來源 → 不能當證據，要重寫。
- 硬約束（規格 §6）：不做 TLS 中間人、不注入憑證、不破解 Client、不改執行檔、
  不攔截訂閱憑證。某個 Client 不支援就標 unsupported，不硬 hack。

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

## 已確定不可行
- **Claude Desktop 用 `ANTHROPIC_BASE_URL`**：Desktop 走的是 Claude apps gateway 的
  `/user/bootstrap` 設定端點 ＋ policy 的 `desktop` 鍵，不是環境變數。
  要支援得實作那個協定（規格由跑起來的 gateway 在 `GET /protocol` 提供，且官方寫明
  不保證無限期向後相容）。列 pending，不阻塞其它段落。
- **把訂閱席次轉成 API credit**：官方規則就是分開計費，這條從一開始就不成立，不再花時間查。
- **Cursor 走私有網段的閘道**：官方說請求「routed through Cursor's servers for final prompt
  building」。要支援就得讓閘道從 Cursor 機房連得到，跟「閘道放私有網段」直接衝突。
  在這點解掉以前不列入支援——**這是架構衝突，不是協定問題，所以查更多協定文件沒用。**

## 證據
- `ANTHROPIC_BASE_URL`「Override the API endpoint to route requests through a proxy or gateway.」
  → **這證明** Claude Code 官方支援指向自架閘道。**證明不了**本機真的跑通過（實測待 W05）。
- `wire_api`「`responses` is the only supported value, and it is the default when omitted.」
  → **這證明** Codex 只走 Responses API。**證明不了** Codex 實際會送哪些欄位——
  Codex 沒有等同 Anthropic 那份的閘道相容契約，這是 W06 的已知風險。
- 「While a gateway credential variable or `apiKeyHelper` is active, a developer's claude.ai
  subscription isn't used... billed per token to whoever owns the credential.」
  → **這證明**席次與 API 計費的邊界主張正確。**證明不了** OpenAI 側的等價聲明（待補）。
- 本機版本：Claude Code `2.1.267`、codex-cli `0.153.0`（`claude --version` / `codex --version`）。
  → **這證明**這兩個版本存在於本機。**證明不了**文件描述的行為在這兩個版本上已生效。

## 來源
- <https://code.claude.com/docs/en/llm-gateway-protocol>（閘道相容契約）2026-09-08
- <https://code.claude.com/docs/en/llm-gateway>（席次邊界、不支援非 Claude 模型）2026-09-08
- <https://code.claude.com/docs/en/env-vars>（環境變數逐條說明）2026-09-08
- <https://code.claude.com/docs/en/claude-apps-gateway>（第一方閘道能力與限制表）2026-09-08
- <https://learn.chatgpt.com/docs/config-file/config-reference>（Codex `model_providers`）2026-09-08

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 子 Agent 查證 | 子 Agent 廣搜 → 主 session 覆核關鍵 URL | 主 session 自己全查 | 全查會把數十頁文件讀進主 context；但「其它 Agent 的報告」不算證據，關鍵條目要自己覆核 |

## 下一步
1. 自行覆核 Cursor 的官方頁面（目前是子 Agent 轉述，證據等級低），重點是
   「請求是否真的經過 Cursor 伺服器」——那決定閘道要不要對外曝露。
2. 補 OpenAI／ChatGPT 側的席次邊界官方原文（目前只有 Anthropic 側齊全）。
3. 席次邊界結論寫進 `docs/security-and-compliance.md`。
4. 實測 Client 對本機閘道往返（等 W05 有 `/v1/messages` 之後）。
   **VS Code 與 Claude Code 兩個都能講 Anthropic Messages，一次驗兩個。**
5. **請 cash 決定**：第一方 Claude apps gateway 的採用與否。見 `docs/feasibility.md` 第四節。
