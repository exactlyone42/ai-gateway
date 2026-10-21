# LOOP: providers-expansion

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#46](https://github.com/DnaBoss/agent-pool-gateway/issues/46)｜分支 `feature/providers-expansion`｜段落 `W16`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 已交付 |
| 優先度 | P2 |
| 前置 | `canonical-layer` |
| 開始 | `2026-09-11T10:43:00+08:00` |
| 結束 | `2026-09-11T11:30:21+08:00`（實耗 47 分） |
| Token | 無法取得精確值：ledger 掃描 19 份 Claude transcript 無紀錄並以 exit 3 拒絕回報 0 |

## 目的
加一個 Provider 不需要改 Core。

## 完成判準
- [x] `GeminiProvider`／`XAIProvider`／`LocalProvider`（vLLM、Ollama）都實作同一組介面：
      `capabilities()`／`health()`／`send()`／`stream()`／`estimate_cost()`。
- [x] **驗收即設計檢查**：新增這三個 Provider 對 `gateway-core` 的 diff **必須為 0 行**。
      不為 0 就代表 Adapter 抽象設計錯了 → 退回 W14 修，不是在這裡繞過。
- [x] 每個 Provider 一組 contract test（含串流、錯誤、用量欄位對映）。
- [x] 每個 Provider 的 token 定義差異寫進 `docs/provider-protocols.md`（不假設可直接相加）。
- [x] 陰性對照：required usage 缺失／格式錯誤時回 502，且 request 與 attempt 都持久化為 failed；
      cached／reasoning 大於 provider total 時 usage contract 變紅。

## 已知
- 規格 §73 就是拿「要不要改 Core」當抽象品質的檢驗標準。
- Local LLM（vLLM／Ollama）通常是 OpenAI 相容協定，但 usage 欄位與能力回報常有缺漏——
  這正好是驗證「能力不匹配不得 silent drop」的好案例。
- 查重：2026-09-11 開工時已有本 loop，但沒有 W16 同題 open issue／PR、遠端 feature branch
  或其它 worktree owner；建立唯一卡 #46 與 `feature/providers-expansion`。
- 現況不是規格原先預期的 Cargo workspace，沒有 `crates/gateway-core`；實際 core 邊界是
  canonical／policy／scheduler／quota／ledger，Provider extension point 在 `src/adapters/provider.rs`。
  W16 必須先建立可機器驗證的 core path baseline，再證明三個 Provider 沒改那些路徑。
- Reviewer 使用獨立 `codex exec`；介面／core 邊界定案前與完整 diff 各審一次，高／中風險
  必須修正或以官方協定與 contract 實測裁定。
- Gemini native REST 使用 `generateContent`／`streamGenerateContent?alt=sse`；token 欄位是
  `promptTokenCount`、`cachedContentTokenCount`、`candidatesTokenCount`、`thoughtsTokenCount`，
  不等同 OpenAI 的 input/output token 分類。
- xAI 官方宣告 inference REST 與 OpenAI 相容，Responses/Chat 使用 Bearer；cached 與 reasoning
  token 分開回報。vLLM 與 Ollama 也提供 OpenAI-compatible Responses/Chat，但能力與 usage
  依版本／模型／`include_usage` 而異，不可把「相容」當完整能力保證。
- 每個 Provider contract 必須涵蓋 capabilities、health plan、request/stream plan、error/usage
  mapping 與 cost estimate；不發真實付費請求，使用本機 deterministic upstream fixture。
- 每段先跑精確 contract test；PR 前跑隔離 DB 的 `./scripts/check.sh all`。AI 可自動合入
  `dev`；只做安全性、W16 必需依賴與 W16 目標，不順手做 W17 fallback。

## 已做
- 2026-09-11T10:43+08:00：確認 W15 PR #45 已合入 `dev`（`8d15b9c`），關閉 #44；完成
  W16 issue／PR／branch 查重，建立卡 #46、Project #2 item 與唯一 feature branch。
- 2026-09-11T10:47+08:00：核對 Google、xAI、vLLM、Ollama 官方文件，記錄 native endpoint、
  authentication、stream、health 與 token usage 差異；未以 OpenAI-compatible 宣稱完全相容。
- 2026-09-11T11:12+08:00：計畫審查完成（Codex session
  `01a08e5b-6eaf-7361-9324-b87bc2c156af`，9,459 tokens）；採納健康檢查必須能實際單次執行、
  W14 修復獨立提交、Local kind 不靠 URL 猜測、Gemini path 必須取得 provider model、介面方法無
  預設實作，以及串流終止／中途錯誤／缺 usage／取消的合約案例。
- 2026-09-11T10:54+08:00：完成 W14 extension 修復：ProviderKind 與 DB provider_type 綁定、
  typed capabilities／health／send／stream／estimate_cost 必要介面、Gemini 動態 target path，既有
  OpenAI／Anthropic 也顯式實作同一契約；精確單元測試 2/2 綠。
- 2026-09-11T11:18+08:00：完成 Gemini native、xAI、vLLM、Ollama adapters、typed profile config、
  單次 health probe、buffered／SSE response 與 usage mapping；admin alias activation 依 provider kind
  選 target operation，不再只認 OpenAI／Anthropic。
- 2026-09-11T11:24+08:00：依 diff reviewer 修正四個實際缺口：Gemini tools 能力改為 false、
  malformed required usage 走 durable failed finalization、admin mapping 補齊新 providers、canonical
  cached／reasoning token 改為互斥類別並在 OpenAI wire 回建總數，避免 quota／ledger／cost 重複計價。
- 2026-09-11T11:30+08:00：隔離 DB 完整 gate exit 0：前端 2/2、Rust 206/206、schema 5/5、
  bans self-test 29/29；core baseline diff exit 0。W16 實作與文件完成。

## 已確定不可行
- 直接檢查 `git diff --stat crates/gateway-core`：repo 沒有該 workspace 路徑，空結果不能當證據。

## 證據
- W14 修復基線：`9100d778096984a19c7ff935e7177aa7bca4b3d7`。
- `git diff --quiet 9100d77 -- src/canonical.rs src/policy.rs src/scheduler.rs src/quota.rs
  src/ledger.rs src/model_registry.rs` → exit 0，W16 provider 實作未改 core。
- Plan review：Codex `01a08e5b-6eaf-7361-9324-b87bc2c156af`（9,459 tokens）。Final review
  `01a08e6d-5dcf-7f01-9896-f1ffc0063cb6` 找到四項缺口並已修；再次 bounded review
  `01a08e81-57ef-7462-92bc-c8a0c0c84ce6` 在 180 秒 timeout 前逐項核對，沒有提出新 finding，
  但未產出正式結語，不能宣稱完整 review 綠。

## 來源
- 規格 §19（Provider 介面）、§73（Phase 6）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| Provider 組態 | typed JSON profile，只存 secret env 名稱 | 任意 provider map／明文 secret | 啟動時 fail closed，避免拼錯與洩密 |
| Local auth | vLLM／Ollama 可選 Bearer | 強制假 secret | 本機部署可無 auth，但不轉送 client credential |
| Usage | canonical token 類別互斥，wire 再回建總數 | 把 total 與 details 直接相加 | quota／ledger／cost 必須只有一套數字且不重複計價 |
| Gemini tools | 先宣告不支援 | 只做 request encoder 就宣稱完整能力 | response functionCall 尚未 round-trip，必須 fail closed |

## 下一步
1. 合併 W16 到 `dev` 並關閉 #46。
2. 啟動 W17 `advanced-routing`，只做健康分數、fallback 與 routing audit，不回頭擴張 provider 協定。
