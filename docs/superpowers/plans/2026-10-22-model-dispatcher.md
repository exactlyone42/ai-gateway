# Model Dispatcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 依硬政策、能力、成本與健康執行已驗證任務圖，讓 Jev 只在合格候選中排序，並回傳可追溯的 `ExecutionBundle`。

**Architecture:** 分派器消費拆解器的固定契約，以拓撲順序及有界併發執行節點。每節點重用既有 policy、quota、scheduler、router、provider adapter 與 ledger；部分失敗保留，不做整合。

**Tech Stack:** Rust、Tokio、SQLx/PostgreSQL、既有 Router/Scheduler/Jev/adapters

**Spec:** `docs/multi-model-orchestration.md`

## Global Constraints

- 前置為 `task-decomposer` 契約完成且凍結。
- 最多 3 並行、每節點一次 Provider 呼叫、沒有隱藏重試。
- Jev 只能排序通過硬條件的候選；錯誤時沿用確定性第一名。
- `local_only` 無合格本地模型必須 fail closed。
- 內容不持久化；父子帳務逐筆可對回。

## Review Focus

- 依賴完成與執行槽釋放競爭時不能啟動兩次。
- Client 取消與 Provider 完成競爭時只能記一次終態與用量。
- Jev 越界或不合格答案不能穿透硬條件。
- 平行節點開始前逐筆 admission，不能超額後補記。
- restart 後 unknown 子請求不能自動重送。

---

### Task 1: 固定執行結果與狀態機

**Files:**
- Create: `src/orchestration/dispatch.rs`
- Modify: `src/orchestration.rs`
- Test: `tests/dispatch_state_machine.rs`

**Interfaces:**
- Consumes: `TaskGraph`
- Produces: `TaskExecutionStatus`、`TaskExecutionResult`、`ExecutionBundle`、`DispatchError`

- [ ] **Step 1: 寫失敗測試**：只允許 pending→running→單一終態；依賴失敗轉 skipped；aggregate usage 精確相加。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test dispatch_state_machine`。
- [ ] **Step 3: 實作型別與純狀態機**：終態為 completed/failed/cancelled/skipped_dependency；Debug 不含內容。
- [ ] **Step 4: 驗證非法二次完成會回錯且不覆寫終態**。
- [ ] **Step 5: 提交**：訊息 `feat: define task dispatch state machine`。

### Task 2: 子任務 admission 與候選排序

**Files:**
- Create: `src/services/task_dispatch.rs`
- Modify: `src/task_classification.rs`
- Modify: `src/jev.rs`
- Modify: `src/services/request_quota.rs`
- Test: `tests/task_dispatch_selection.rs`

**Interfaces:**
- Consumes: `select_worker(&AppState, &CanonicalRequest, &TaskNode) -> Result<WorkerSelection, DispatchError>`
- Produces: `select_for_role(..., ExecutionRole) -> Result<WorkerSelection, DispatchError>`；消費已凍結的 `ExecutionRole`，回傳 `WorkerSelection { model, provider, routing_reason, admission }`

- [ ] **Step 1: 寫失敗測試**：能力、資料政策、tenant policy、quota、健康依序排除；Jev 只看剩餘集合；LocalOnly 完全不呼叫 Jev；其餘情況逾時、429、越界回確定性第一名；Worker 與 Integrator 角色皆可選。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test task_dispatch_selection`。
- [ ] **Step 3: 抽出既有選模能力**：共用 request quota 候選流程，不複製政策；改吃 canonical 能力，不讀 Client JSON；Jev 收明確候選及 task 摘要。
- [ ] **Step 4: 驗證 local-only 陰性案例**：只有線上候選時 Provider 呼叫為 0，錯誤為 `no_local_model`。
- [ ] **Step 5: 提交**：訊息 `feat: select workers within hard policy`。

### Task 3: 有界任務圖執行器

**Files:**
- Create: `src/orchestration/executor.rs`
- Modify: `src/orchestration.rs`
- Modify: `src/services/task_dispatch.rs`
- Test: `tests/task_graph_execution.rs`

**Interfaces:**
- Consumes: `execute_graph(&AppState, &CanonicalRequest, &TaskGraph, CancellationToken) -> Result<ExecutionBundle, DispatchError>`
- Produces: 完整或部分成功的 `ExecutionBundle`

- [ ] **Step 1: 寫失敗測試**：平行峰值恰為 3；依賴順序正確；一項失敗不丟獨立結果；下游跳過；每節點最多一呼叫。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test task_graph_execution`。
- [ ] **Step 3: 實作 ready queue**：容量 3 semaphore 與 bounded channel；不得建無界 spawn。
- [ ] **Step 4: 驗證陰性限制**：要求峰值 4 時測試紅；失敗 Provider 呼叫仍為 1。
- [ ] **Step 5: 提交**：訊息 `feat: execute bounded task graphs`。

### Task 4: 取消、deadline 與重啟語意

**Files:**
- Modify: `src/orchestration/executor.rs`
- Modify: `src/routes/messages.rs`
- Modify: `src/services/openai.rs`
- Test: `tests/task_dispatch_cancellation.rs`

**Interfaces:**
- Consumes: parent cancellation、request deadline、節點估時
- Produces: cancelled/skipped 終態與已完成結果；無孤兒呼叫

- [ ] **Step 1: 寫失敗測試**：取消時未開始為零呼叫、進行中收到取消、已完成保留；deadline 後不啟動新節點。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test task_dispatch_cancellation`。
- [ ] **Step 3: 接既有連線取消**：同一 token 傳到 transport；unknown 不重送。
- [ ] **Step 4: 跑競爭測試 100 次**：每次終態與用量只落一次。
- [ ] **Step 5: 提交**：訊息 `feat: cancel task graph execution safely`。

### Task 5: 全圖 deadline 與 Client heartbeat

**Files:**
- Modify: `src/orchestration/executor.rs`
- Modify: `src/services/streaming.rs`
- Modify: `src/routes/messages.rs`
- Modify: `src/services/openai.rs`
- Test: `tests/orchestration_deadline.rs`

**Interfaces:**
- Consumes: `CanonicalRequest.deadline`（空值時接收時間加 600 秒）、`stream_heartbeat_interval`、各階段剩餘時間
- Produces: 整段有界 deadline；stream 編排期間每 30 秒送 SSE comment heartbeat

- [ ] **Step 1: 寫失敗測試**：慢拆解與慢 worker 仍先收到 heartbeat；deadline 不足時不啟動下一節點；非 stream 不注入內容。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test orchestration_deadline`。
- [ ] **Step 3: 復用既有 streaming heartbeat**：不發假的 message event，不重設 600 秒總契約。
- [ ] **Step 4: 陰性對照**：停用 heartbeat 時 60 秒模擬 Client 測試必須紅。
- [ ] **Step 5: 提交**：訊息 `feat: bound orchestration latency`。

### Task 6: 父子帳務、查詢與觀測

**Files:**
- Modify: `src/ledger.rs`
- Modify: `src/admin/reports.rs`
- Modify: `docs/observability.md`
- Test: `tests/orchestration_ledger.rs`

**Interfaces:**
- Consumes: parent request 與 `TaskExecutionResult`
- Produces: orchestration_id、task_id、execution_role、parent_request_id、狀態、模型、用量、成本與延遲 metadata

- [ ] **Step 1: 寫失敗測試**：以 `(tenant_id, orchestration_id, execution_role, task_id, attempt=1)` 唯一鍵保證父項合計等於子項；role 為 NOT NULL，decomposer 與 integrator 分別使用固定 task ID `__decomposer__`、`__integrator__`；取消／完成競爭及重複 completion 不重複用量；跨 tenant 隔離；schema 無內容欄位。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test orchestration_ledger`。
- [ ] **Step 3: 使用拆解器已建立的 append-only execution receipt，補 Worker 寫入與 Admin 查詢**：不修改舊 migration 或既有 ledger 行。
- [ ] **Step 4: 跑專項與 gate**：專項測試後跑 `./scripts/check.sh gate`。
- [ ] **Step 5: 提交**：訊息 `feat: account for orchestrated model calls`。

### Task 7: 測試入口與整段驗收

**Files:**
- Create: `src/routes/orchestration_test.rs`
- Modify: `src/routes.rs`
- Modify: `docs/operations.md`
- Modify: `loops/model-dispatcher.loop.md`
- Test: `tests/orchestration_test_route.rs`

**Interfaces:**
- Consumes: 僅 development bootstrap 可用的固定 `TaskGraph`
- Produces: `ExecutionBundle` JSON；正式 Client 路徑仍不啟用

- [ ] **Step 1: 寫失敗測試**：production 與一般 tenant 拒絕；development 可執行；任意 raw prompt 欄位拒絕。
- [ ] **Step 2: 實作最小測試入口**：沿用 RBAC 與既有錯誤出口。
- [ ] **Step 3: 用 mock、本地及授權線上模型驗收**：沒付費授權時只跑 mock/本地並明記。
- [ ] **Step 4: 跑 `./scripts/check.sh all` 並更新 LOOP**。
- [ ] **Step 5: 提交**：訊息 `test: verify multi-model dispatch end to end`。
