# LOOP: cross-provider

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#44](https://github.com/DnaBoss/agent-pool-gateway/issues/44)｜分支 `feature/cross-provider`｜段落 `W15`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 已交付（PR #45，`8d15b9c`） |
| 優先度 | P2 |
| 前置 | `canonical-layer` |
| 開始 | `2026-09-11T08:50:31+08:00` |
| 結束 | `2026-09-11T10:42:05+08:00` |
| Token | 無法取得精確值：現有腳本只掃 Claude transcript，Codex 執行窗回傳 exit 3 |

## 目的
Anthropic 協定的 Client 能用 OpenAI 模型，反之亦然——而且能力轉不過去時會明說，不是默默吞掉。

## 完成判準（**逐項驗收、逐項宣布，不准一次宣布相容**）
- [x] ① 文字：`Messages → OpenAI`、`Responses → Anthropic` 各一組 contract test。
- [x] ② 串流：SSE 事件序列逐項比對；無法如實轉換的方向在 quota 前拒絕。
- [x] ③ 工具呼叫：定義、呼叫、結果回填三段都比對。
- [x] ④ 結構化輸出：只接受已驗證的 JSON Schema 子集，其餘拒絕。
- [x] ⑤ 推理／extended thinking：確認 hidden state 不可無損轉換，穩定拒絕且不扣 quota。
- [x] 轉不過去的能力**禁止 silent drop**。只允許三種處置：拒絕、明示能力降級、改路由到支援的模型。
- [x] **陰性對照**：移除 provider residual 防護後精確 contract test 變紅，還原後綠。

## 已知
- **這一段有官方政策風險，不只是技術問題**：Anthropic 明文
  「doesn't support routing Claude Code to non-Claude models through any gateway」
  （<https://code.claude.com/docs/en/llm-gateway>，2026-09-08）。
  技術上做得到，但「Claude Code ＋ 非 Claude 模型」不在 Anthropic 支援範圍。
  → 這段的產出必須標註此風險，不得當一般功能宣傳；要不要做由 cash 決定。
- 規格 §72 明確給了順序：文字 → 串流 → 工具 → 結構化輸出 → 推理。
- 規格 §18 列出不一定能一對一轉換的能力：reasoning／extended thinking／prompt cache／
  tool calling／structured outputs／computer use／image／provider hosted tools／MCP 語意。
- 這段開始前，`docs/provider-protocols.md` 必須已記錄兩邊的實際事件形狀（W05／W06 的產出）。
- 查重：2026-09-11 開工時只有本 loop；沒有同題 open issue／PR、遠端 branch 或其他
  worktree owner。建立唯一卡 #44 與 `feature/cross-provider`。
- Reviewer 使用獨立 `codex exec`；能力矩陣／轉換策略定案前與完整 diff 各審一次，高／中風險
  必須修正或以規格及 contract 實測裁定。
- 計畫審查裁定：採納 exact target-operation 三方授權、轉譯驗證必須早於 quota、跨 Provider
  header 隔離、嚴格 SSE 狀態機、gateway response ID 與 target usage accounting；W15 v1 對
  reasoning 明確 400，不宣稱可跨協定保留 hidden state。
- 支援矩陣只含 `Anthropic Messages ↔ OpenAI Responses`。Chat Completions、Count Tokens、
  background／conversation state、image、opaque/provider extension、未知 JSON Schema keyword
  皆拒絕；不做 silent drop 或 warning-only degradation。
- 每項先跑精確 contract test；PR 前跑 `./scripts/check.sh all`、loops 與 `git diff --check`。
  每輪記錄已做、證據、不可行、trade-off、下一步；完成後 AI 可自行合入 `dev`。
- 外部依賴先自行打通；只做安全性、W15 必需依賴與 W15 目標，不順手擴增第三方 Provider。

## 已做
- 2026-09-11T08:50+08:00：完成 cashloop 查重，建立卡 #44、Project #2 item、唯一 feature 分支。
- 2026-09-11T09:07+08:00：完成 `ValidatedTranslationPlan`；model row、provider row、受信 profile
  必須共同允許 target operation，且只有驗證後候選可進 quota。
- 2026-09-11T09:13+08:00：完成雙向 request／buffered response encoder-decoder；client vendor
  header 不跨 Provider，target ID 只進 ledger，client 只見 gateway namespace ID。
- 2026-09-11T09:17+08:00：完成雙向 SSE strict observer/encoder、唯一終止、順序／索引／大小
  限制；native relay 保持 raw bytes 與 unknown-event tolerance。
- 2026-09-11T09:21+08:00：雙向 buffered 端到端、reasoning pre-quota 陰性整合測試通過；
  176 條 Rust 測試全綠，完整關卡只被舊本機測試 DB migration checksum 漂移擋住。
- 2026-09-11T10:11+08:00：第一、二輪 diff review 的 2H/8M 與 1H/6M 全數修正；補齊
  target usage、response envelope、唯一 ID、strict topology、stream 方向限制與 schema 欄位保留。
- 2026-09-11T10:27+08:00：第三輪 review 的 1H/3M 與兩個 partial closure 全數修正；generated
  tool call 可回填，token-limit partial response 與 durable incomplete 一致，system 順序、instructions、
  citations/annotations、stream initial tool input 均有明確保留或 pre-quota/fail-closed 行為。
- 2026-09-11T10:40+08:00：第四輪 closure review 找到 2M；修正 incomplete item 與零參數 tool
  stream round-trip，39 條 adapter tests 全綠；第五輪精準 closure review PASS（67,962 tokens）。
- 2026-09-11T10:42+08:00：隔離新 DB 跑 `./scripts/check.sh all` 全綠；移除產品路徑
  `unreachable!` 後，前端 2、Rust 197、schema 5、bans self-test 29 全數通過。

## 已確定不可行
- Chat Completions → Anthropic：其 `n`／choice topology 不可無損映成單一 Messages response。
- Count Tokens → OpenAI：OpenAI Responses 沒有等價 count-only operation。
- Reasoning／extended thinking：兩邊 hidden continuation/signature 語意不同；W15 v1 一律 400。
- Anthropic client → OpenAI target 串流：OpenAI 到 terminal 才提供 input usage，無法如實產生
  Anthropic `message_start`，因此在 quota 前拒絕；反向串流可支援。
- Claude Code 指向非 Claude 模型不在 Anthropic 官方 gateway 支援政策內；只交付 API 能力。

## 證據
- `cargo test adapters::`：39 條 contract 測試全綠。
- 隔離 DB 的雙向 buffered incomplete E2E：partial output 保留，2 requests/2 attempts 均記
  `failed` + `provider_response_incomplete`。
- reasoning rejection 實測：upstream 0 call，`quota_windows=0`、`quota_reservations=0`。
- silent-drop mutation：移除 provider residual 防護後，精確 contract test 單獨變紅；還原後綠。
- `cargo clippy --all-targets --all-features --offline -- -D warnings`：綠。
- 第五輪獨立 reviewer：PASS；兩個 closure 精準測試各 1/1 通過。
- 隔離新 DB 的 `./scripts/check.sh all`：exit 0，Rust 197/197、schema 5/5、前端 2/2、
  bans self-test 29/29，0 skipped。
- `./scripts/token-ledger.sh --slug cross-provider`：exit 3；正確時間窗內只掃 Claude transcript，
  無 Codex usage record，因此拒絕把 0 當量測值。

## 來源
- 規格 §18、§72（Phase 5）、§75（contract test）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 相容範圍 | 兩個 generate dialect 的可證交集 | 泛稱 OpenAI-compatible | 能力不等價時拒絕比靜默改語意安全 |
| 驗證時機 | candidate 進 quota 前完成 plan | 扣款後才 encode | 拒絕不能消耗 quota 或產生 reservation |
| 串流 | target strict decode → canonical → source stateful encode | 逐字串替換／raw cross relay | 能驗順序、ID、終止、usage 與大小界線 |
| 推理 | 穩定 400 | 猜測映射 effort/budget 或丟 thinking | hidden state 與簽章不可無損轉換 |

## 下一步
1. W16 `providers-expansion` 已由卡 #46 與唯一 feature branch 接手。
