# LOOP: test-harness-load

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#50](https://github.com/DnaBoss/agent-pool-gateway/issues/50)（部分覆蓋於 [#19](https://github.com/DnaBoss/agent-pool-gateway/issues/19)）｜分支 `feature/test-harness-load`｜段落 `W18`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 完成 |
| 優先度 | P1 |
| 前置 | 階段一：`openai-native`；階段二：`scheduler-admission` |
| 開始 | `2026-09-11T11:58:39+08:00` |
| 結束 | `2026-09-11T12:15:15+08:00`（16 分 36 秒） |
| Token | 無法取得精確值：Claude transcript ledger 在本時窗找不到 Codex usage，拒絕回報 0 |

## 目的
拿數字證明「不互串、不漏、不爆、不餓死」，不是靠綠燈。

## 完成判準（階段一，W06 之後）
- [x] 可控 simulation：正式 parser/observer 驗 SSE split、慢消費、取消、malformed、partial、
      oversize、tool call；429／500／逾時走一次性 production-path 測試，不對同一遠端做 N 次呼叫。
- [x] 100 條同時程序內串流狀態：100 個正式 `response.created` 後同步；ID 0 錯配、90 完成、
      10 取消、active 歸零、FD delta 0；三輪 RSS 記錄於 journal。Provider 併發由 scheduler batch 驗證。
- [x] 故障矩陣七項各有預期結果：timeout／429／500／串流中斷／PostgreSQL 不可用／
      Client 斷線由一次性 production-path 測試；OTel 不可用沿用 W07 三種實測，未宣稱本輪重跑。
- [x] 程序內 overhead 基準已入 journal：first-event P50/P95/P99、queue-wait P50/P95/P99、
      CPU、RSS/VmHWM、FD。它不是端到端 TTFT，也不代表真實 HTTP/DB/OTLP 吞吐。

## 完成判準（階段二，W13 之後）
- [x] 混合負載：10 互動 premium ＋ 30 互動 economy ＋ 30 背景 premium ＋ 30 批次 economy。
      quality 只是標籤；實際機制是 workload-family reservation。100 筆全完成、前 20 筆中 15 筆
      interactive、60 筆 background/batch 無餓死、deadline 與 queue-full 回傳正確錯誤。
- [x] 機制對照：provider limit 3/6 的 peak 為 3/6；reservation 關閉後 interactive 失去保留槽；
      aging 關閉後同一 batch dispatch position 由 1 退到 5；共用 observer 會抓到 stream 串線。

## 已知
- 2026-09-11 開工基線已有 215 條 Rust 測試；route tests 各自有 mock provider，但沒有可重跑、
  可量測 100 串流與混合 scheduler 負載的共同 harness，journal 仍無數字。
- 全域規約：**任務核心目標未經實測驗證前不寫測試**。所以這一段排在 W06 之後，
  不在 W02～W05 期間先磨測試。
- 壓測的本業是量吞吐，不是寫單元測試（全域規約）。
- 參考做法：`/home/cash/work_space/private/essence/docs/LOADTEST_JOURNAL.md` 的紀錄格式。
- 上位規則禁止一個動作對同一 DB／遠端服務做 N 次，含壓測；W18 採單一程序內批次 simulation，
  不建立 100 次 HTTP 或 DB 呼叫。這能驗併發狀態機與資源界線，不能宣稱真實網路吞吐。
- 查重：既有 #19 是 HA／SLO 廣義營運卡；沒有同題 issue／PR／remote branch／worktree owner。
  已建立唯一卡 #50 與 `feature/test-harness-load`。
- Reviewer 使用獨立 Codex；計畫與完整 diff 各審一次。AI 可自動合入 `dev`；只做 W18。
- 計畫審查要求 formal parser/observer/scheduler、修正 premium 保留槽的錯誤假設、明列不可宣稱範圍；
  diff 審查的五個 medium 已全修：同步點在正式 event 後、完成與 ID 真斷言、provider cap 獨立對照、
  三輪 RSS、queue wait 與 first-event 命名修正、aging 走真實 dispatch。

## 已做
- 2026-09-11T11:58+08:00：W17 PR #49 已合入 `dev`（`eda3ac3`）；完成 W18 查重、建立卡
  #50 並加入 Project #2，從最新 `dev` 建唯一 feature branch。
- 2026-09-11：加入 provider read-idle timeout（預設 120 秒、每次成功 read 重置）與一次性 timeout 測試。
- 2026-09-11：加入 100-stream/100-work scheduler 程序內 harness、故障 parser matrix、正式 scheduler
  陰性對照；release 隔離量測三輪並寫入 `docs/loadtest-journal.md`。

## 已確定不可行
- 在「一個動作不得對同一遠端服務做 N 次」的上位規則下，不能以 100 次 HTTP／PostgreSQL／OTLP
  呼叫做傳統壓測；本輪只證明程序內狀態機與資源界線，不宣稱真實網路吞吐或服務容量。
- 模型 TTFT 含 provider 推理與網路，本程序內 harness 只能量第一個正式 SSE event 的處理延遲。
- 三輪 RSS 趨穩不是任意長時間記憶體上限；VmHWM 也是歷史高水位差，不是精準 allocation accounting。

## 證據
- `cargo test w18_in_process_load_harness --release --lib -- --nocapture --test-threads=1`：peak stream
  state 100；90 completed／10 cancelled；100 unique IDs／0 mismatch；active after 0；FD delta 0。
- 同輪：first-event 1/2/4 us；CPU 1,187,490 ns；RSS +16 KiB；三輪 after RSS
  9,532/9,876/9,900 KiB。scheduler wait 20,638/37,910/40,041 us；provider peak 3。
- 隔離 DB 執行 `./scripts/check.sh all` exit 0：frontend 2/2、Rust 217/217、schema 5/5、
  bans self-test 29/29。
- 完整環境、原始欄位、機制對照、七項 fault mapping 與不能證明的範圍見
  `docs/loadtest-journal.md`。量測程式碼 SHA `030cd71`。
- Reviewer：計畫 session `01a08e9e-b935-7cb0-b3ac-9466b19a8f26`（25,452 tokens）；
  diff session `01a08ea6-c6db-7a10-b49e-efd76e69683b`（40,036 tokens），五個 medium 全修。

## 來源
- 規格 §75（測試種類）、§76（併發測試）、§77（排程測試）、§78（故障測試）、§79（效能測試）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 壓測邊界 | 同程序 batch simulation＋一次性外部邊界測試 | 100 次遠端/DB 呼叫 | 遵守上位禁止重複打同一服務規則 |
| 排程切面 | workload-family reservation；quality 只記標籤 | 不存在的 premium reserved slot | 與正式 scheduler 實作一致 |
| timeout | read-idle timeout，每個成功 read 重置 | 固定總串流壽命 | 長串流只要持續有資料就不應被切斷 |

## 下一步
1. 提交文件與審查修正、推 PR #51；CI 綠後合入 `dev`、關閉 #50。
2. 從最新 `dev` 接續 W19 `loops/process-lifecycle.loop.md`。
