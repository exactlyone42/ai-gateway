# LOOP: quota-ratelimit

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#13](https://github.com/DnaBoss/agent-pool-gateway/issues/13)｜分支 `feature/quota-ratelimit`｜段落 `W12`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P1 |
| 前置 | `policy-engine`, `usage-ledger` |
| 開始 | `2026-09-11T05:57:56+08:00` |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
超量會被擋或被降級，而且擋的理由查得到、看得懂。

## 完成判準
- [x] 限制種類齊全：每分鐘請求數／每分鐘估算 token／每小時 token／每日 token／
      併發請求數／每日成本／每月成本。
- [x] 維度齊全：User／Device／Client／Agent／Credential／Model／Provider。
- [x] soft limit：走降級階梯（premium → advanced → standard → economy）或排隊。
- [x] hard limit：直接拒絕，回穩定的 error code，並記錄「哪一條限制擋的」。
- [x] 估算用量在上游前算，provider-reported 用量回來後對帳修正（兩者不互相覆寫）。
- [x] 配額視窗（`quota_windows`）可查、可重算，不靠記憶體狀態。
- [x] **日／月邊界的時區寫死並測**：跨日、跨月、以及夏令時間（若採用的時區有）。
      「每日成本上限」在沒有定義時區以前是沒有意義的字串。
- [x] **多實例語意已定案**：第一版只允許單一 gateway 實例；升到多實例前，W19 必須先把
      即時計數器移進 PostgreSQL，否則兩個 replica 會讓上限無聲翻倍。
- [x] **陰性對照 1**：把某條限制的門檻設成 0 → 該類請求必須全被擋，且 error code 指得出是哪一條。
- [x] **陰性對照 2**：重啟閘道 → 已用掉的配額不得歸零（證明狀態不只在記憶體）。

## 已知
- W12 資料面已實作速率與配額控制；後續改動必須保留先 reservation、再 durable ledger、最後上游的順序。
- W08 擁有 Quota 的 tenant-safe、immutable-version draft CRUD schema；W12 接手啟用、視窗與
  enforcement，不能另建平行資料表，啟用／換版必須留下 audit。
- 規格 §30 明確要求「估算 token／分鐘」也要有限制，不只請求數。
- 規格 §31：soft／hard 的差別由 Admin 設定，不是寫死。
- 第一版**不做**分散式計數器（規格 §82 禁止 Redis Cluster）。單機做對，測到瓶頸再談。
- `docs/task-ledger.md` 已於 2026-09-08 決定第一版單實例；卡 #13 的 Redis-backed 舊描述與
  現行 repo 決策衝突，依專案守則採單實例即時閘門＋PostgreSQL 持久視窗，不引入 Redis。
- 卡 #13 的 company/team/principal 維度也早於現行 identity schema；本 repo 沒有 team entity，
  W12 依既定 User／Device／Client／Agent／Credential／Model／Provider 維度，不自建平行 team。
- 查重：沿用既有卡 #13 與本 loop；開工時沒有同名本機／遠端分支、open PR 或其它 worktree
  owner，因此建立 `feature/quota-ratelimit`，不開第二張卡。
- Reviewer 使用獨立 `codex exec`；計畫定案與完整 diff 各審一次。高／中嚴重度須修正或以
  規格與實測裁定。每段跑相關 Rust/DB 測試與陰性對照，提交前 `./scripts/check.sh all`。

## 已做
- `2026-09-11T05:58+08:00` W11 merge `e7bcc4c`、收工 commit `1eaea5b` 後接手；確認卡、loop、
  分支、PR 與 worktree ownership，無重複實作，建立 feature 分支。
- `2026-09-11T06:03+08:00` 計畫獨立 Codex review 完成（2,427 tokens）：6 high、2 medium。
  採納重放冪等、immutable quota/price snapshot、未知成本 fail-closed、hard 優先與故障矩陣；
  併發改用單實例 RAII lease，不以會過期的 DB lease 管長串流。
- `2026-09-11T06:32+08:00` 完整 diff 獨立 Codex review 完成（282,367 tokens）：4 high、7 medium。
  逐項修正 hard quota 上界、單實例鎖、admit/reconcile race、cache cost、soft floor、capability fallback、
  count_tokens、credential family、ghost reservation、Admin 可見性與 decision 上限；修正後皆有對應測試。
- `2026-09-11T06:47+08:00` 全新 `apg_w12_final` 跑 `./scripts/check.sh all`：前端 2、Rust 133、
  schema 3+2、bans self-test 29/29 全綠。Playwright 1440px/390px 均無水平溢位，手機 quota
  limits 改為人類可讀逐行標籤；短暫 Vite 已停止。

## 已確定不可行
- quota reservation 與本機 durable ledger spool 無法放進同一個 PostgreSQL transaction；強行改成
  DB-only 會破壞既有「DB 暫斷仍不漏帳」契約。改以同一 request UUID 關聯，且兩者 durable 後
  才能打上游；中間 crash 只留下保守估算 reservation，絕不留下已打上游但無 ledger 的請求。
- reviewer 假設有跨 tenant 的 global/provider/model quota；現行 schema 的三者都有 tenant_id，
  沒有跨 tenant scope。tenant advisory lock 足以序列化同 tenant admission 與 quota activation。
- 用 DB lease expiry 管 concurrency 會把仍在跑的長串流誤釋放；第一版已明定單實例，改用
  行程內 RAII lease，process crash 時請求本身也同時結束，不需 stream heartbeat 打 DB。

## 證據
- hard=0 route 回 `429 quota_exceeded`、ledger 記 `requests_per_minute`，provider attempts=0。
- 新 `QuotaEngine` 仍讀到上一個 instance 的 PostgreSQL window；第二個 process 無法取得 startup lock。
- 估算缺 output cap 時採 model maximum 並乘 Chat `n`；image/tool/service tier 或缺 cache 價格時 fail closed。
- provider actual replay 兩次只產生一個 reconciliation event；transaction barrier 證明 ingest 等同一 tenant lock。
- 七種 scope、六種 persistent limit、200 字 quota key 同時套用，共 42 windows 且 decision 可持久化。
- `recompute_quota_windows()` 從 immutable reservation/reconciliation 把注入的錯誤值 99 重建回 3。

## 來源
- 規格 §30（限制種類）、§31（soft／hard）、§42（估算 vs 實際）、§54（政策後台）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 第一版計數 | 單實例即時計數＋PostgreSQL 持久視窗 | Redis／假裝記憶體計數可跨 replica | 符合 §82 與已記錄決策，避免無聲放大上限 |
| 原子邊界 | quota DB reservation 與 ledger spool 共用 request UUID；兩者 durable 後才上游 | 假裝 PG 與檔案可同交易 | crash gap 最多保守多算，不會少記 provider 使用 |
| 併發 | startup DB singleton lock＋process-local RAII lease | 需週期 heartbeat 的 DB lease | 強制單實例，取消/結束自動釋放，不對 DB 做事件迴圈 |

## 下一步
1. 提交 PR #39，等待 CI 判定後合入 `dev`，關閉卡 #13。
2. 接續 W13 `scheduler-admission`；佇列只消費 W12 admission 結果，不另建平行 quota counter。
