# LOOP: gateway-blueprint

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#20](https://github.com/DnaBoss/agent-pool-gateway/issues/20)｜分支 `feature/gateway-blueprint`｜段落 `W00`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P0 |
| 前置 | 無 |
| 開始 | `2026-09-08T21:45+08:00` |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
把「一份 92 節的規格」變成「18 個可獨立審查的段落 ＋ 每段落一份活的現況檔 ＋ 一套機器可檢查的規範」，
並先回答「這件事到底做不做得到」。

## 完成判準
- [x] `docs/work-breakdown.md`：18 段落、依賴圖、既有看板卡對照、明確不做清單。
- [x] `docs/feasibility.md`：結論 ＋ 官方 URL ＋ 查證日期 ＋ 每條的邊界說明。
      **VS Code／Cursor／Codex Desktop 三列仍是 `未查證`**，標成 pending 而不是猜。
- [x] `loops/*.loop.md`：19 份，全部列進 `loops/README.md`，全部對映到看板卡或標「開工時建」。
- [x] `docs/engineering-standards.md`：14 節，每條有機器檢查或寫明驗收動作。
- [x] `docs/task-ledger.md`：記帳規則 ＋ token 實測分佈 ＋ 跨段落 trade-off 表。
- [x] `./scripts/check.sh all` 全綠（fmt／clippy／test 1 passed 0 skipped／loops／bans）。
- [x] **陰性對照（loop 檔關卡）三條全紅**：缺段落標題、主體超 200 行、沒進索引。
- [x] **陰性對照（bans 關卡）五條全紅**：生產路徑 unwrap、硬編模型名、程式碼裡出現
      可觀測性後端名稱、高基數 metric label、`mod.rs`。
      加上一條**正對照**：同樣的構件放進 `#[cfg(test)]` 模組**不會**誤報。

## 已知
- repo 非空。既有 5 支 Rust 檔共 262 行：`src/main.rs`（只有一行啟動）、
  `src/startup.rs`（設定載入 ＋ JSON 日誌 ＋ 服務生命週期）、`src/routes.rs`（路由 ＋ request-id 層）、
  `src/gateway.rs:37`（`/v1/chat/completions` 整包轉發 LiteLLM）、`src/config.rs`、`src/error.rs`。
- 既有實作與新規格的**四個落差**：非串流（`src/gateway.rs:64` 先 `response.bytes().await` 收完才回）、
  上游是 LiteLLM、沒有資料庫、沒有身分模型。
- 既有 CI 三關：`cargo fmt --check`、`cargo clippy --all-targets --all-features -D warnings`、
  `cargo test --all`（`.github/workflows/ci.yml:20`）。
- 看板是 DnaBoss Project #2，狀態欄位叫 `Workflow`（不是 `Status`），由
  `scripts/board-sync.sh` 在 issue／PR 事件時只允許向前推進。
- repo 原本沒有 `dev` 分支，只有 `main` 與 `feat/T-06-virtual-key-store`。
- Claude Code 的對話紀錄（`~/.claude/projects/*/*.jsonl`）每筆 assistant 訊息帶
  `timestamp` / `requestId` / `gitBranch` / `agentName` / `message.usage`，
  子 Agent 各自一個檔但同一個專案目錄 → **token 用量可按時間窗實測，不必估**。
- 同一則訊息在串流過程會被寫入多次，必須用 `requestId` 去重，否則會重複計算。

## 已做
- `2026-09-08` 讀完既有 4 支 Rust 檔、5 份文件、2 個 workflow、board-sync 腳本，確認落差。
- `2026-09-08` 建 `origin/dev` 當整合分支（原本沒有），開卡 #20、分支 `feature/gateway-blueprint`、
  草稿 PR [#21](https://github.com/DnaBoss/agent-pool-gateway/pull/21)。
- `2026-09-08` 寫 `CLAUDE.md`（只放參數 ＋ 文件索引 ＋ 四條硬約束）與 `loops/_TEMPLATE.loop.md`。
- `2026-09-08` 寫 `scripts/token-ledger.sh`：按時間窗、去重、分 Agent 統計 token。實測可用（見證據）。
- `2026-09-08` 寫 `scripts/check.sh`：`gate`（快）／`all`（PR 前）／`loops`（loop 檔紀律）。
- `2026-09-08` 寫 `docs/work-breakdown.md`：18 段落 ＋ 依賴圖 ＋ 既有 15 張卡的對照與作廢判定。
- `2026-09-08` 寫 19 份 loop 檔 ＋ 索引，並把 loop 檔紀律做成機器檢查（三條陰性對照全紅）。
- `2026-09-08` 自行覆核可行性關鍵四條，寫 `docs/feasibility.md`、重寫
  `docs/client-compatibility.md`。查到 Anthropic 有官方閘道相容契約，直接變成 W05 的完成判準。
- `2026-09-08` 寫 `docs/engineering-standards.md`，並把其中五條做成 `check.sh bans` 關卡。
  這個關卡抓到既有程式碼的第一個真實違規（`src/gateway.rs` 生產路徑的 `.expect`），已修掉。
- `2026-09-08` CI 改成呼叫 `./scripts/check.sh all`，本機與 CI 共用同一份關卡定義。
- `2026-09-08` 改寫 `docs/architecture.md`（三個 Plane、五個概念不准混、故障模型）、
  更新 `README.md`、修 `docs/security-and-compliance.md` 的 LiteLLM 敘述。
- `2026-09-08` 看板整理：#6 作廢並關閉（附取代路徑），#16 拆兩段、#8 縮範圍、#4 改順序，各留說明。
- `2026-09-09` 把啟動流程拆成「設定與生命週期」與「路由」兩塊，`main` 只剩一行啟動。
  行為不變（實測 `/healthz` 200、未授權 401、`x-request-id` 仍回傳、JSON 日誌照舊），
  順手修掉六處指到舊 `src/main.rs` 行號的文件引用。

## 已確定不可行
- **沿用 `Status` 當看板欄位名**：這個 repo 的欄位實際叫 `Workflow`（repo variable `FIELD_NAME=Workflow`），
  照 skill 預設值寫會找不到欄位。
- **靠 `gitBranch` 當 token 歸屬的主鍵**：子 Agent 記的是**產生時**的分支，切分支後不會更新
  （實測：`compat-research` 記在 `DnaBoss/StartWork`，而工作已在 `feature/gateway-blueprint`）。
  改用 loop 檔宣告的時間窗當主鍵，分支與 Agent 只當分項。

## 證據
- token 實測（`./scripts/token-ledger.sh --since 2026-09-08T13:45`，UTC）：
  3 個 Agent／59 次呼叫／**billed 5,922,469 tokens**，其中 `cache_read` 5,405,939（91.3%）、
  `cache_write` 467,550、`output` 48,862。
  → **這證明**：token 成本的主體是 context 重讀，不是輸出；每起一個子 Agent 就多一份 cache_write
  （123k ＋ 205k）。**證明不了**實際金額（不同 token 種類費率不同，本腳本不算錢）。
- `./scripts/check.sh loops` → `loop files ok`，但當時 `loops/` 只有範本。
  → **這證明**腳本跑得起來，**證明不了**它抓得到違規。陰性對照未做，列在完成判準。

## 來源
- 規格本體：使用者於 2026-09-08 提供的 92 節 Implementation Specification。
- 工程品質參考：`/home/cash/work_space/private/essence`（Rust workspace 規約）、
  `/home/cash/orca/workspaces/ai-mall/*`（loop 檔紀律、看板 SOP）、
  `/home/cash/orca/workspaces/Helios/以真理之名`。
- 方法論：全域 `~/.claude/CLAUDE.md`、`start-work` skill。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 整合分支 | 新建 `dev`，`main` 留給正式 | 直接在 `main` 上疊 18 段落 | 18 段落分批合，需要一個能反覆合併又不觸發部署的落點 |
| 段落粒度 | 18 段（可獨立審查） | 7 段（照規格的 Phase 切） | Phase 太大，一份 loop 檔的主體會破 200 行，代表任務該拆 |
| Phase 4–7 的 loop 檔 | 先建、內容從簡 | 等前置完成才建 | 建了才進得了索引與依賴圖；但不預寫還沒有事實依據的內容 |
| 看板卡 | 只對映既有卡，缺的等開工才建 | 一次補開 9 張新卡 | 18 張躺在 backlog 的卡比沒有卡更糟——沒人看、還會被當成有人在做 |
| 既有 `.expect` | 直接修掉 | 加白名單讓關卡先過 | 生產路徑的 panic 捷徑屬於安全性（收斂原則第①類），不是「順手撿的問題」 |
| token 記帳 | 從對話紀錄實測 | 事後估算 | 估算不可稽核；實測有 `requestId` 可去重、可重跑 |

## 下一步
1. 收 `compat-research` 對 VS Code／Cursor／Codex Desktop 的查證，補進
   `docs/client-compatibility.md`（目前是 `未查證`）。
2. 收 `quality-extract` 的三專案規範萃取，比對 `docs/engineering-standards.md` 有沒有漏。
3. **請 cash 決定範圍**：第一方 Claude apps gateway 已覆蓋 SSO／群組模型白名單／支出上限／
   OTLP，是否把本專案收斂到它沒有的四項（多 Provider、排程、可查詢帳本、Admin UI）。
   見 `docs/feasibility.md` 第四節。這個決定會改變 W08／W09 的範圍。
4. PR #21 轉 ready → 合 `dev`。
5. 開工 W02 `workspace-foundation`（無前置，關鍵路徑起點）。
