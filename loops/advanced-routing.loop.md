# LOOP: advanced-routing

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#48](https://github.com/DnaBoss/agent-pool-gateway/issues/48)（健康路由／fallback 原在作廢的 [#6](https://github.com/DnaBoss/agent-pool-gateway/issues/6)）｜分支 `feature/advanced-routing`｜段落 `W17`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 已交付 |
| 優先度 | P2 |
| 前置 | `providers-expansion` |
| 開始 | `2026-09-11T11:32:12+08:00` |
| 結束 | `2026-09-11T11:56:59+08:00`（25 分） |
| Token | 無法取得精確 token：ledger 未辨識 Codex transcript；reviewer 回報見「已做」 |

## 目的
Provider 變慢或壞掉時自動換路，而且換路的理由與代價查得到。

## 完成判準
- [x] 上位安全規則要求每個 logical request 最多一次 Provider call；每個有 attempt 的 request
      在初始 durable snapshot 記 `attempt_kind=initial` 與 `routing reason`，不得把跨請求換路冒充 retry／fallback。
- [x] transport failure／transient timeout／指定 5xx／overload 只更新被動健康狀態；下一個請求
      可在呼叫前換 Provider，原請求不得再打遠端。
- [x] 已開始串流、已產生 token 成本、工具有副作用、非幂等結果不明都不得觸發第二次呼叫。
- [x] 健康感知／負載感知／成本感知／配額感知路由，各自可獨立開關。
- [x] 自動品質路由（最後才做）。
- [x] **陰性對照**：讓 mock provider 失敗，該請求 call count 仍為 1；下一請求改選健康 Provider。

## 已知
- 規格 §74 明確：這些是最後才加的，不要一開始做 ML Router。
- 規格 §41 要求 Retry 與 Fallback 的遙測分開，否則事後分不清是誰的問題。
- 規格 §40 列出重試的四個危險點，每一個都要有對應測試。
- 2026-08-15 專案規則明文禁止同一動作對遠端做 N 次，且特別禁止失敗重試；它高於 W17
  舊判準。因此 W17 的 fallback 定義為「下一請求依被動訊號預先換路」，不是同請求第二次 call。
- 查重：2026-09-11 開工時已有本 loop，但沒有同題 open issue／PR、遠端 feature branch或
  其它 worktree owner；建立唯一卡 #48 與 `feature/advanced-routing`。
- Reviewer 使用獨立 `codex exec`；attempt／retry 邊界定案前與完整 diff 各審一次，高／中風險
  必須修正或以既有規格與負面對照裁定。
- 每段先跑精確測試，PR 前以隔離 DB 跑 `./scripts/check.sh all`。AI 可自動合入 `dev`；只做
  安全性、W17 必需依賴與 W17 目標，不順手做 W18 load harness 或 W19 lifecycle。

## 已做
- 2026-09-11T11:32+08:00：確認 W16 PR #47 已合入 `dev`（`2d6239b9`），關閉 #46；完成
  W17 issue／PR／branch 查重，建立卡 #48、Project #2 item 與唯一 feature branch。
- 2026-09-11T11:36+08:00：計畫審查完成（Codex
  `01a08e87-ccaa-70b3-9dec-6f35790d340e`，9,143 tokens）；採納明確改寫舊驗收、每請求只有
  一筆 initial attempt、無訊號時維持穩定順序、狀態更新帶完成時間戳，以及 reason 與 attempt
  同一 durable envelope。
- 2026-09-11T11:48+08:00：完成被動路由引擎、五個獨立開關、單次 provider call、durable
  routing history 與 Admin 呈現；完整 gate 首次 212/212 綠。Diff reviewer（37,332 tokens）提出
  cooldown、reason score 與不完整價格三個 Medium，全部修正。
- 2026-09-11T11:56+08:00：精簡複審（Codex
  `01a08e96-e8be-70a3-892c-afe29b438d95`，62,162 tokens）提出 half-open 並行、初始 reason
  未落盤、stream error 未降健康三個 Medium；全部修正並各補測試，最終 gate 215/215 綠。

## 已確定不可行
- 同請求 timeout／5xx 後 retry 或 fallback：違反專案「對遠端 N 次與重試全面禁止」規則；即使
  不用語法 loop、手寫第二次 call 仍然違規。

## 證據
- `DATABASE_URL=... ./scripts/check.sh all`：前端 2/2、Rust 215/215、schema 5/5、bans 29/29，
  loop／dead-file／client compatibility／log secret gates 全綠。
- 端到端陰性對照：第一請求 mock primary 回 503，primary call count 仍為 1；第二請求選 secondary，
  durable providers=`[primary,secondary]`、reasons=`[registry,health]`、max attempts=1。
- half-open 原子 claim：同一 Provider cooldown 到期只允許一個 probe；平行 claim 在本機回
  `provider_unavailable`，不呼叫遠端。串流 `provider_stream_error` 會讓下一請求換路。
- 初始 spool `reserved` 檔直接反序列化，provider call 前已有 routing reason；刪掉該寫入時測試紅。

## 來源
- 規格 §40（retry）、§41（fallback）、§74（Phase 7）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 失敗處理 | 被動健康 + 下一請求預先換路 | 同請求 retry/fallback | 遵守遠端單次呼叫上位規則 |
| half-open | 原子 claim，無可用路徑就本機 503 | 同時放行多個 probe | 防止故障 Provider 被並行探測放大 |
| 持久化 | 初始 request policy snapshot | 等 completion 才補 reason | 程序崩潰仍可解釋選路 |

## 下一步
1. W18 `test-harness-load`：用併發、隔離與壓力證據驗證 W00-W17 的跨模組契約。
2. W19 `process-lifecycle`：驗證排空、訊號與串流部署生命週期。
