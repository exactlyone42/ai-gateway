# LOOP: process-lifecycle

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#52](https://github.com/DnaBoss/agent-pool-gateway/issues/52)｜分支 `feature/process-lifecycle`｜段落 `W19`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P0 |
| 前置 | `anthropic-native`（要有真的長命串流才驗得出來） |
| 開始 | `2026-09-11T12:21:54+08:00` |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
部署一次不會砍掉全公司正在跑的 Claude Code session，也不會讓行程永遠退不掉。

## 完成判準
- [ ] **有上限的排空**：收到 SIGTERM 後 data-plane admission 關閉但 listener 留著回答 health；
      既有串流繼續，active 歸零即退出，超過 `drain_timeout` 才由 connection handle 強制關閉。
- [ ] 排空期間 `/ready` 回非 200（讓負載平衡器移走），`/health` 仍 200（讓編排器別重啟它）。
- [ ] 被強制關閉的串流要留下記錄（哪些請求、進行到哪、用量估算），不是無聲消失。
- [ ] Request body 與 application header bytes 有大小上限，超過回 413；transport parser 另有較高的
      固定 buffer/count 上限，超過時依 HTTP 規格回 431，兩層都用 raw TCP 驗證。
- [ ] 所有上游呼叫有逾時；沒有任何一條路徑可以永久掛著。
- [x] 多實例語意已由 W12 定案：第一版單實例，PostgreSQL advisory lock 在 bind 前阻擋第二行程；
      多實例前必須改成跨實例 durable counters，不能把記憶體上限乘上實例數。
- [x] 配額日／月邊界已由 W12 定義為 UTC，跨日跨月測試已在完整 gate 通過。
- [ ] **陰性對照 1**：起一條不主動結束的串流 → 觸發與 SIGTERM 相同的 controller；期限前仍可
      收後續 event，排空後的新 TCP 連線得到 `/ready` 503、`/health` 200。直接關連線時本測試要紅。
- [ ] **陰性對照 2**：把 `drain_timeout` 設成 0 → 行程必須立刻退出且留下被砍串流的記錄。

## 已知
- W07 已補 SIGTERM 與 telemetry flush，但仍直接交給 axum 無上限 graceful shutdown；Axum 0.8
  把 connection task detach，drop server future 不會終止它們。W19 改用 `axum-server` handle。
- W12 已解文件矛盾並實作單實例 advisory lock；W19 只移除「W19 必須改分散式」的過時文字，
  不在這一段新增一次 DB 往返的 distributed counter。
- W05/W06/W18 已有 endpoint body limits 與 provider read-idle timeout；W19 補 application/transport
  header bounds、provider connect timeout，以及 process-level 強制終止。
- 強制終止不能只在 stream relay `select!`：client 不讀時 body 不再被 poll。必須由 server handle
  關 connection，讓 body/reservation Drop；Drop 依 force token 標 `shutdown_timeout`。
- provider attempt 必須在呼叫前 durably checkpoint，否則 force 發生在 headers 前會被誤認成
  「沒打 provider」並把 quota 歸零。partial usage 不可當 final actual；取消時保留估算 reservation。
- `Reservation::Drop` 是 blocking finalize；shutdown 必須等待 completion signal，時間到要明確報未完成，
  Tokio runtime 本身也要有最後的 bounded shutdown。
- 這一段是反方審查抓出來的缺口：`grep -riE 'shutdown|drain|排空|滾動'` 在 `docs/` 與 `loops/`
  原本**零命中**。18 個段落沒有人擁有「帶著長命串流的行程生命週期」。

## 已做
- 2026-09-11T12:21+08:00：W18 PR #51 已合入 `dev`（`5b3d5c4`）；查重無同題 open
  issue／PR／remote branch／worktree owner，建立唯一卡 #52 與本分支。
- 2026-09-11：計畫 reviewer Codex session `01a08eaf-ef05-7671-8c1d-953f002db9db`
  （76,575 tokens）提出六個 high、一個 medium；全部採納進上方設計。

## 已確定不可行
- **靠 axum 的預設 graceful shutdown**：它等 in-flight 結束，而長命 SSE 沒有「結束」，
  等於無上限等待。必須自己管排空期限。
- **只 cancel relay future**：下游背壓時 body 不被 poll，取消分支永遠不執行；需要 connection handle。
- **固定 sleep 後假設已落盤**：fsync/finalize 有自己的完成條件，必須等 completion signal 並設上限。
- **取消時用 partial actual 對帳 quota**：output 尚未完整，會低估；取消保留原估算 reservation。

## 證據
- （待填）

## 來源
- 規格 §63（故障隔離）、§64（背壓）、§65（安全：body 上限、逾時）。
- 反方審查（2026-09-08）第 5 項。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| Server | `axum-server` connection handle | 複製 Axum/Hyper server internals | 現成支援 hard shutdown 且可調 transport builder |
| Drain listener | 排空期繼續 listen，只拒絕 data plane | signal 當下關 listener | 同一 port 才能實測 ready 503 / health 200 |
| Multi-instance | 第一版單實例 advisory lock | 本輪做 distributed counter | W12 已確保正確性，跨實例計數需另量 DB 成本 |
| Interrupted quota | partial usage 留證據，quota 保留估算 | 用不完整 actual 下修 | 不低估尚未收到的 output/cost |

## 下一步
1. 加 lifecycle/admission/active guard，改用可強制關連線的 server handle 與 bounded runtime shutdown。
2. 送 provider 前 checkpoint attempt；forced Drop 標 `shutdown_timeout`，等待 finalizer completion。
3. 補 connect/header bounds、兩個 process lifecycle 對照與 raw TCP header 邊界。
