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
- [ ] `docs/feasibility.md`：每個 Client 一條結論，附官方 URL ＋ 查證日期 ＋ 邊界說明。
- [ ] `loops/*.loop.md`：每段落一份，全部列進 `loops/README.md`。
- [ ] `docs/engineering-standards.md`：每條規範可機器檢查，或寫明驗收動作。
- [ ] `docs/task-ledger.md`：每段落一列（開始／結束／token／trade-off）。
- [ ] `./scripts/check.sh all` 全綠。
- [ ] **陰性對照**：把某個 loop 檔的 `## 證據` 段落刪掉，`./scripts/check.sh loops` 必須變紅。
      （目前的 `loop files ok` 是在 0 個 loop 檔的情況下印的，那是橡皮圖章。）

## 已知
- repo 非空。既有 4 支 Rust 檔共 241 行：`src/main.rs`（路由 ＋ JSON 日誌 ＋ request-id 層）、
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
| token 記帳 | 從對話紀錄實測 | 事後估算 | 估算不可稽核；實測有 `requestId` 可去重、可重跑 |

## 下一步
1. 收 `compat-research` 的查證結果 → 寫 `docs/feasibility.md`、更新 `docs/client-compatibility.md`。
2. 收 `quality-extract` 的三專案規範萃取 → 寫 `docs/engineering-standards.md`。
3. 補完 18 份 loop 檔與 `loops/README.md` 索引。
4. 做 `check.sh loops` 的陰性對照（刪一個段落標題，確認變紅）。
5. 更新 `docs/architecture.md`：LiteLLM 從執行期依賴改成「不使用」，補三個 Plane 的邊界。
6. `./scripts/check.sh all` 全綠 → PR #21 轉 ready → 合 `dev`。
7. 為 W01～W18 開卡並把卡號回填各 loop 檔。
