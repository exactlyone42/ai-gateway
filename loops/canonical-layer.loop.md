# LOOP: canonical-layer

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#42](https://github.com/DnaBoss/agent-pool-gateway/issues/42)｜分支 `feature/canonical-layer`｜段落 `W14`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 完成 |
| 優先度 | P2 |
| 前置 | `anthropic-native`, `openai-native` |
| 開始 | `2026-09-11T07:46:15+08:00` |
| 結束 | `2026-09-11T08:46:05+08:00` |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
協定轉換從 N×M 變成 N+M，而且**現有行為一位元都不准變**。

## 完成判準
- [x] 內部型別齊全：`CanonicalRequest` / `CanonicalResponse` / `CanonicalStreamEvent` /
      `CanonicalMessage` / `CanonicalToolDefinition` / `CanonicalToolCall` / `CanonicalUsage` /
      `CanonicalError`。
- [x] `CanonicalRequest` 帶得動政策與排程要用的欄位（gateway_request_id／trace_id／session_id／
      principal／requested_model／requested_quality／workload_class／urgency／budget_class／
      deadline／tools／tool_choice／stream／max_output_tokens／provider_options／metadata）。
- [x] Protocol Adapter（Client 側）與 Provider Adapter（上游側）分離，**不得**互相依賴。
- [x] `provider_options.<vendor>` 保留原廠能力，**不做最小公約數抽象**。
- [x] **黃金測試**：W05／W06 既有的 contract test 全部沿用、逐位元比對，不得放寬任何一條。
- [x] **陰性對照**：故意在 canonical 轉換裡丟掉一個欄位 → 黃金測試必須變紅。

## 已知
- 這一段是**純重構**：行為保持的重構要與功能變更**分開提交**（全域規約）。
- 規格 §16 明確禁止 N×M 轉換；§18 明確禁止 silent drop。
- 前置的 W05／W06 必須先有可靠的 contract test，否則這段沒有安全網——
  所以 W05／W06 的完成判準裡就要求 contract test。
- 查重：2026-09-11 開工時只有本 loop；沒有同名遠端分支、open PR、同題 issue 或其他
  worktree owner。建立唯一卡 #42 與 `feature/canonical-layer`。
- Reviewer 使用獨立 `codex exec`；型別／依賴方向定案前與完整 diff 各審一次，高／中風險須修正
  或用既有規格及逐位元實測裁定。
- 每輪跑相關 contract tests；PR 前跑 `./scripts/check.sh all`、loops 與 `git diff --check`。
  記錄已做、證據、不可行、trade-off、下一步；完成後 AI 可自行合入 `dev`。
- 外部依賴先自行打通；只做安全性、必要依賴與 W14 目標，不順手加入跨 Provider 功能。

## 已做
- 2026-09-11T08:01+08:00：盤點 W05/W06 路徑與 raw byte 契約；既有同協定 request、response、
  SSE 都只允許 model 改寫，其餘逐位元直通。
- 2026-09-11T08:01+08:00：獨立 Codex 計畫審查找出 1 High/6 Medium；採納完整 stream
  vocabulary、decode/enrich 兩階段、opaque Principal、scoped residual、redacted sidecar 與文件修正。
- 2026-09-11T08:01+08:00：新增 canonical 型別、Protocol Adapter、core native dispatcher 與
  Provider Adapter；Anthropic/OpenAI 四個 operation 都走一次 decode，政策/配額讀同一份 document。
- 2026-09-11T08:01+08:00：七條 adapter 測試、兩條原始 SSE 端到端契約通過；OpenAI 429
  收緊為原始 bytes sentinel 比對。
- 2026-09-11T08:35+08:00：完整 diff 審查先後找出 source/target operation 混用、structured
  output 型別不足、alias 衝突、finish reason 遺失與測試假綠；全部修正並補精確事件序列。
- 2026-09-11T08:43+08:00：目標 Provider Adapter 現在自行選 native operation、URI、認證與
  header allowlist；兩條 client route 共用同一 transport，同協定仍保留原 path/query/body。
- 2026-09-11T08:46+08:00：聚焦複審 PASS；完整 `check.sh all` 通過，W14 收工。
- 2026-09-11T08:46+08:00：`token-ledger.sh` 找不到本輪 Codex transcript，依工具規則拒絕把
  0 當測量值；精確 token 無法取得。

## 已確定不可行
- 把 W06 的五種帳務觀察事件直接當完整 canonical stream：缺文字、reasoning、tool argument
  delta 與 finish reason，W15 無法使用；已改為 canonical usage 並補完整事件語彙。
- 只靠 native fast path 宣稱 canonical lossless：刪掉 semantic mapping 仍會轉送原始 bytes、測試假綠；
  改用獨立 projection golden 與 test-only drop fault。
- Provider Adapter 自己判斷 source×target：會重新長回 N×M；改由 core dispatcher 比對 operation，
  後續審查證明仍不夠；最終改成 source operation 只投影為 neutral operation，目標 adapter
  自行決定 native operation 與完整上游 request。

## 證據
- `cargo clippy --workspace --all-features -- -D warnings`：通過。
- `cargo test adapters:: -- --nocapture`：15 passed、0 failed、0 ignored。
- Responses／Anthropic raw stream contract：各 1 passed、逐位元 request/SSE 比對未放寬。
- Root 與 nested residual 陰性對照都先斷言基準存在，再清除欄位並斷言檢查失敗；不靠
  `should_panic`，無法以原本就丟欄位的狀態假綠。
- 多輸出 stream 測試精確比對 `(0, stop)`、`(1, length)`、completion hint 與唯一 terminal。
- `./scripts/check.sh all`：Rust 167、schema 5、前端 2、bans self-test 29/29，全數通過。
- 獨立 Codex 聚焦複審：`PASS`，0 High／Medium；`git diff --check` 通過。

## 來源
- 規格 §16（Adapter 架構）、§17（Canonical 型別）、§18（Provider 能力）、§19（Provider 介面）、§71（Phase 4）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 同協定 wire | canonical view + native sidecar | 重編碼所有 JSON/SSE | 保持 W05/W06 一位元不變 |
| 敏感資料 | opaque ID、不可序列化、redacted debug | CanonicalRequest derive Serialize/Debug | prompt/PII 不得進遙測或 spool |
| 依賴方向 | protocol/provider 各自只依賴 canonical，target adapter 產完整 request | adapter 互相 import 或 source×target 分支 | 維持 N+M |

## 下一步
1. PR #43 CI 綠後合入 `dev`、關卡。
2. 接續 W15 `loops/cross-provider.loop.md`，依文字、串流、工具、結構化輸出、推理逐項驗收。
