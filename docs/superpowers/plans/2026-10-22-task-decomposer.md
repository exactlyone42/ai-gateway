# Task Decomposer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在統一請求格式上辨識複合需求並產生可驗證的 `TaskGraph`，簡單或不安全的請求維持既有單模型路徑。

**Architecture:** 拆解器位於 protocol decode/enrich 之後、policy admission 之前，只讀 `CanonicalRequest`。模型只提出圖，程式負責結構與安全驗證；第一版以 shadow 模式執行，不觸發子任務。

**Tech Stack:** Rust、Tokio、Serde、SQLx/PostgreSQL、既有 canonical adapters

**Spec:** `docs/multi-model-orchestration.md`

## Global Constraints

- 最多 8 節點、依賴鏈最多 2 層；節點不可再次拆解。
- 工具互動、token-count、不安全原生欄位、逾時、低信心與錯誤輸出一律 `Direct`。
- Prompt、子任務內容與模型輸出不得進 log、trace、ledger 或 spool。
- 第一版只 shadow，不改路由、不增加工作模型呼叫。
- 不引入 LiteLLM、MCP 或 Client 專屬邏輯。

## Review Focus

- 同義 Anthropic/OpenAI 請求須產生等價判斷，不能讀原始 JSON 猜 Client。
- prompt injection 要求放寬限制時，驗證器仍須拒絕。
- 循環、重複 ID、缺少依賴與三層圖須在任何子任務呼叫前失敗。
- 簡單要求不能被硬拆。
- shadow 只存圖形 metadata，不存 objective 或回應內容。

---

### Task 1: 固定契約與純驗證器

**Files:**
- Create: `src/orchestration.rs`
- Create: `src/orchestration/contracts.rs`
- Create: `src/orchestration/graph.rs`
- Modify: `src/lib.rs`
- Test: `tests/task_graph_contract.rs`

**Interfaces:**
- Consumes: `CanonicalRequest`、`CapabilityRequirements`、`CanonicalUsage`
- Produces: `TaskGraph`、`TaskNode`、`DataPolicy`、`OutputContract`、`ExecutionRole::{Decomposer, Worker, Integrator}`、`DecompositionDecision`、`validate_graph(&TaskGraph) -> Result<(), GraphValidationError>`

- [ ] **Step 1: 寫失敗測試**：合法平行圖與兩層圖；重複 ID、未知依賴、循環、9 節點、3 層、空 objective、資料政策降級各自斷言精確錯誤。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test task_graph_contract`，預期型別不存在。
- [ ] **Step 3: 實作最小契約與驗證器**：使用拓撲排序驗循環及最長路徑；錯誤不含輸入內容。
- [ ] **Step 4: 確認通過**：同一命令全部 PASS。
- [ ] **Step 5: 提交**：訊息 `feat: define task graph contract`。

### Task 2: 正規化資料政策與能力判斷

**Files:**
- Modify: `src/orchestration/contracts.rs`
- Modify: `src/model_registry.rs`
- Modify: `src/routes/messages.rs`
- Modify: `src/services/openai.rs`
- Test: `tests/canonical_capability_requirements.rs`

**Interfaces:**
- Consumes: tenant hard policy、request opt-in、`CanonicalRequest`
- Produces: `DataPolicy::{LocalOnly, OnlineAllowed, OnlineRequired}`、`CapabilityRequirements::from_canonical(&CanonicalRequest)`

- [ ] **Step 1: 寫失敗測試**：三種入口的等價 canonical request 得到相同能力；LocalOnly 不可被節點放寬；OnlineRequired 不可覆蓋 LocalOnly。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test canonical_capability_requirements`。
- [ ] **Step 3: 實作 canonical 判斷並遷移既有入口**：保留 raw 版本只作對照，orchestration 不得呼叫它。
- [ ] **Step 4: 驗證工具歷史**：messages 含 `ToolCall` 或 `ToolResult` 時 decomposition eligibility 必為 false。
- [ ] **Step 5: 提交**：訊息 `feat: normalize orchestration policy inputs`。

### Task 3: 建立固定案例與評分器

**Files:**
- Create: `tests/fixtures/task_decomposition_cases.json`
- Create: `tests/task_decomposition_eval.rs`
- Create: `src/orchestration/decomposition_eval.rs`
- Modify: `src/orchestration.rs`

**Interfaces:**
- Consumes: `DecompositionDecision` 與 fixture 的 required/forbidden labels
- Produces: `DecompositionScore`、`score_decomposition(...)`

- [ ] **Step 1: 寫失敗測試**：至少 30 個複合案例與 10 個簡單／工具案例；缺必要項、加入禁止項、誤拆簡單案例均變紅。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test task_decomposition_eval`。
- [ ] **Step 3: 實作確定性評分**：只比較 fixture label，不讓模型評自己。
- [ ] **Step 4: 驗證陰性對照**：刪一個必要 label 時門檻紅，復原後綠。
- [ ] **Step 5: 提交**：訊息 `test: add decomposition quality corpus`。

### Task 4: 實作模型邊界與安全解析

**Files:**
- Create: `src/orchestration/decomposer.rs`
- Create: `src/services/task_decomposition.rs`
- Modify: `src/orchestration.rs`
- Modify: `src/state.rs`
- Test: `tests/task_decomposer_service.rs`

**Interfaces:**
- Consumes: `decompose(&AppState, &CanonicalRequest) -> Result<DecompositionDecision, GatewayError>`
- Produces: 經 schema 與 `validate_graph` 驗證的 decision

- [ ] **Step 1: 寫失敗測試**：簡單、工具定義、強制工具、歷史 ToolCall/ToolResult 與不支援投影 bypass；合法 JSON 成圖；壞 JSON、逾時、低信心、越界圖回 `Direct`；logger 無 sentinel prompt。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test task_decomposer_service`。
- [ ] **Step 3: 實作 bounded request**：5 秒逾時、64 KiB 回應上限、嚴格 JSON schema；只記 reason code。
- [ ] **Step 4: 驗證通過及機密陰性對照**：故意記 sentinel 時檢查須紅，復原後綠。
- [ ] **Step 5: 提交**：訊息 `feat: add bounded task decomposer`。

### Task 5: 接上 shadow、metadata 與報表

**Files:**
- Create: `migrations/20261022HHMMSS_orchestration_config.sql`
- Create: `migrations/20261022HHMMSS_orchestration_execution.sql`
- Create: `migrations/20261022HHMMSS_task_decomposition.sql`
- Modify: `src/services/request_policy.rs`
- Modify: `src/ledger.rs`
- Modify: `src/admin/reports.rs`
- Modify: `docs/observability.md`
- Test: `tests/task_decomposition_schema.rs`
- Test: `tests/task_decomposition_shadow.rs`

**Interfaces:**
- Consumes: `DecompositionDecision`
- Produces: tenant mode disabled/shadow（預設 disabled）；通用 execution receipt 以 `(tenant_id, orchestration_id, execution_role, task_id, attempt)` 唯一；以及 decision、reason、node_count、max_depth、elapsed_us、contract_version，不存 objective

- [ ] **Step 1: 寫失敗測試**：預設 disabled 不呼叫拆解模型；shadow 不改工作模型候選與呼叫數；LocalOnly 拆解只用本地且不呼叫 Jev，無本地模型回 Direct；execution receipt 使用 `execution_role=decomposer` 與固定 task ID `__decomposer__`；跨 tenant 查不到；schema 沒有內容欄位。
- [ ] **Step 2: 確認兩支測試先失敗**。
- [ ] **Step 3: 加 config/decomposition migration 與 shadow 接線**：拆解呼叫重用硬政策與 admission；DB、無合格拆解模型或拆解錯誤不擋主請求，但記 reason code。
- [ ] **Step 4: 跑專項與 gate**：`cargo test --test task_decomposition_schema --test task_decomposition_shadow`，再跑 `./scripts/check.sh gate`。
- [ ] **Step 5: 提交**：訊息 `feat: observe task decomposition in shadow mode`。

### Task 6: 品質驗收

**Files:**
- Modify: `docs/task-ledger.md`
- Modify: `loops/task-decomposer.loop.md`

**Interfaces:**
- Consumes: 固定案例、shadow 報表與測試輸出
- Produces: 是否可供分派器使用的明確判定

- [ ] **Step 1: 跑固定案例**：required recall ≥90%、invented=0、simple direct=100%。
- [ ] **Step 2: 跑 `./scripts/check.sh all`**：記錄覆蓋數及未跑的正式實送。
- [ ] **Step 3: 更新 LOOP**：未取得自然流量不得宣稱正式品質完成。
- [ ] **Step 4: 提交**：訊息 `docs: record task decomposer acceptance`。
