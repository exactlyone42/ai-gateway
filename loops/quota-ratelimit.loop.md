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
- [ ] 限制種類齊全：每分鐘請求數／每分鐘估算 token／每小時 token／每日 token／
      併發請求數／每日成本／每月成本。
- [ ] 維度齊全：User／Device／Client／Agent／Credential／Model／Provider。
- [ ] soft limit：走降級階梯（premium → advanced → standard → economy）或排隊。
- [ ] hard limit：直接拒絕，回穩定的 error code，並記錄「哪一條限制擋的」。
- [ ] 估算用量在上游前算，provider-reported 用量回來後對帳修正（兩者不互相覆寫）。
- [ ] 配額視窗（`quota_windows`）可查、可重算，不靠記憶體狀態。
- [ ] **日／月邊界的時區寫死並測**：跨日、跨月、以及夏令時間（若採用的時區有）。
      「每日成本上限」在沒有定義時區以前是沒有意義的字串。
- [x] **多實例語意已定案**：第一版只允許單一 gateway 實例；升到多實例前，W19 必須先把
      即時計數器移進 PostgreSQL，否則兩個 replica 會讓上限無聲翻倍。
- [ ] **陰性對照 1**：把某條限制的門檻設成 0 → 該類請求必須全被擋，且 error code 指得出是哪一條。
- [ ] **陰性對照 2**：重啟閘道 → 已用掉的配額不得歸零（證明狀態不只在記憶體）。

## 已知
- 現況沒有任何速率或配額控制。
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

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §30（限制種類）、§31（soft／hard）、§42（估算 vs 實際）、§54（政策後台）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 第一版計數 | 單實例即時計數＋PostgreSQL 持久視窗 | Redis／假裝記憶體計數可跨 replica | 符合 §82 與已記錄決策，避免無聲放大上限 |

## 下一步
1. 盤點既有 Quota draft schema、request reserve/finish 與 policy/router 邊界，送計畫反方 review。
2. 配額視窗 schema ＋ 原子 reservation/reconciliation 路徑。
3. soft 降級階梯、hard 拒絕與兩個陰性對照。
