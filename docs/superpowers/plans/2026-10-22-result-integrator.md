# Result Integrator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 由另行分派的整合模型把任務圖與子結果組成唯一、可驗證且可轉回原 Client 協定的 `CanonicalResponse`。

**Architecture:** 整合器消費原始 canonical request、凍結的 task graph 與 execution bundle。程式在模型前確認完整性與大小，在模型後確認覆蓋、矛盾揭露、schema 與引用；最後接回既有 response/stream adapters。

**Tech Stack:** Rust、Tokio、Serde、SQLx/PostgreSQL、既有 canonical response/stream adapters

**Spec:** `docs/multi-model-orchestration.md`

## Global Constraints

- 前置為 task-decomposer 與 model-dispatcher 契約及測試入口完成。
- 整合模型由同一分派流程選擇，`execution_role=integrator`，不能繞過硬政策。
- 子結果視為不可信資料，不能改寫原始要求。
- 必要結果缺少時不得聲稱完整；內容不得持久化。
- Client 工具迴圈第一版維持單模型路徑。

## Review Focus

- 子結果內的 prompt injection 不得改變整合規則。
- 結果矛盾時必須揭露，不能任選其一後假裝一致。
- structured output 不合 schema 時須明確失敗。
- aggregate usage 與公開 model 欄位須符合三種 Client 協定且不洩漏內部模型。
- 串流開始後的錯誤事件須符合原入口協定。

---

### Task 1: 固定整合輸入與前置檢查

**Files:**
- Create: `src/orchestration/integration.rs`
- Modify: `src/orchestration.rs`
- Test: `tests/integration_input.rs`

**Interfaces:**
- Consumes: `IntegrationInput { original, graph, execution }`
- Produces: `PreparedIntegration`、`IntegrationError`、`prepare_integration(...)`

- [ ] **Step 1: 寫失敗測試**：orchestration ID 不同、未知或重複 result、必要節點缺失、超過 context 上限各回穩定錯誤；非必要失敗可列缺口。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test integration_input`。
- [ ] **Step 3: 實作純檢查與安全封裝**：以 task ID 標界結果並標為 untrusted evidence；Debug 不含內容。
- [ ] **Step 4: 加 injection 陰性案例**：惡意子結果仍保持資料角色。
- [ ] **Step 5: 提交**：訊息 `feat: validate result integration inputs`。

### Task 2: 選擇與呼叫整合模型

**Files:**
- Create: `src/services/result_integration.rs`
- Modify: `src/orchestration/integration.rs`
- Test: `tests/integrator_selection.rs`

**Interfaces:**
- Consumes: `integrate(&AppState, PreparedIntegration) -> Result<CanonicalResponse, IntegrationError>`
- Produces: 一次 integrator Provider 呼叫的 canonical response 與 ledger receipt

- [ ] **Step 1: 寫失敗測試**：候選須有足夠 context、reasoning、structured output；local_only 不得選線上；Jev 只排合格集合。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test integrator_selection`。
- [ ] **Step 3: 復用 dispatcher `select_for_role(..., ExecutionRole::Integrator)`**：不修改分派器契約，不複製候選過濾與 admission。
- [ ] **Step 4: 驗證 Jev 失敗仍只呼叫一個 integrator Provider 且只記一次帳**。
- [ ] **Step 5: 提交**：訊息 `feat: dispatch a constrained integration model`。

### Task 3: 後置驗證與部分結果語意

**Files:**
- Create: `src/orchestration/output_validation.rs`
- Modify: `src/services/result_integration.rs`
- Test: `tests/integration_output.rs`

**Interfaces:**
- Consumes: integrator `CanonicalResponse`、TaskGraph、缺口、原始 structured output
- Produces: `validate_integrated_response(...) -> Result<CanonicalResponse, IntegrationError>`

- [ ] **Step 1: 寫失敗測試**：必要 task 無引用、隱藏缺口、未揭露矛盾、超出輸出限制、schema 不符均失敗；完整結果通過。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test integration_output`。
- [ ] **Step 3: 實作確定性檢查**：task coverage 用 metadata IDs；schema 沿用既有 validator 或已裝依賴。
- [ ] **Step 4: 陰性對照**：關閉任一 coverage 檢查時 fixture 必須紅，再復原。
- [ ] **Step 5: 提交**：訊息 `feat: validate integrated model results`。

### Task 4: 串接完整資料面與協定出口

**Files:**
- Create: `src/orchestration/coordinator.rs`
- Modify: `src/services/request_policy.rs`
- Modify: `src/services/request_quota.rs`
- Modify: `src/routes.rs`
- Modify: `src/adapters/response.rs`
- Modify: `src/adapters/stream.rs`
- Test: `tests/orchestration_protocol_roundtrip.rs`

**Interfaces:**
- Consumes: tenant feature flag 與 `CanonicalRequest`
- Produces: 單模型或 decomposer→dispatcher→integrator 的單一 response/stream

- [ ] **Step 1: 寫失敗測試**：flag 關閉時 native bytes 不變；開啟後三種入口等價且出口正確；工具 bypass；取消傳整張圖。
- [ ] **Step 2: 確認先失敗**：跑 `cargo test --test orchestration_protocol_roundtrip`。
- [ ] **Step 3: 加 coordinator 與 flag**：只有有效複合圖進多模型；整合前不送語意事件，最終串流沿用 adapter。
- [ ] **Step 4: 驗證 wire 回公開別名及 aggregate usage，內部模型只留 Admin metadata**。
- [ ] **Step 5: 提交**：訊息 `feat: orchestrate canonical requests end to end`。

### Task 5: 旁路比較、啟用與回退

**Files:**
- Create: `migrations/20261022HHMMSS_orchestration_enforced.sql`
- Modify: `src/admin/resources.rs`
- Modify: `src/admin/reports.rs`
- Modify: `web/src/pages.tsx`
- Modify: `docs/operations.md`
- Modify: `loops/result-integrator.loop.md`
- Test: `tests/orchestration_config.rs`

**Interfaces:**
- Consumes: tenant mode disabled/shadow/enforced 與版本化門檻
- Produces: 單模型對照的成功率、品質、p50/p95 latency、Provider/Jev cost、部分失敗率

- [ ] **Step 1: 寫失敗測試**：沿用拆解器建立且預設 disabled 的 tenant config；operator 可 shadow、只有 admin 可 enforced；CAS、audit、跨 tenant、回退受既有 RBAC 保護。
- [ ] **Step 2: 以新 ALTER migration 加入 enforced，再實作設定與報表**：不得修改拆解器已合併 migration；shadow 不把第二份答案回 Client，只存評分與用量 metadata。
- [ ] **Step 3: 跑三 Client 固定案例**：各含簡單、複合、矛盾、部分失敗、取消及工具案例。
- [ ] **Step 4: 跑 `./scripts/check.sh all` 與陰性對照**：local_only 選線上及漏 task 引用必須紅。
- [ ] **Step 5: 更新 LOOP 並提交**：無部署或付費授權時只記本機／mock；訊息 `docs: record result integration acceptance`。
