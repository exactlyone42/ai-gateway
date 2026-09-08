# LOOP: quota-ratelimit

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 #TBD｜分支 `feature/quota-ratelimit`｜段落 `W12`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P1 |
| 前置 | `policy-engine`, `usage-ledger` |
| 開始 | — |
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
- [ ] **陰性對照 1**：把某條限制的門檻設成 0 → 該類請求必須全被擋，且 error code 指得出是哪一條。
- [ ] **陰性對照 2**：重啟閘道 → 已用掉的配額不得歸零（證明狀態不只在記憶體）。

## 已知
- 現況沒有任何速率或配額控制。
- 規格 §30 明確要求「估算 token／分鐘」也要有限制，不只請求數。
- 規格 §31：soft／hard 的差別由 Admin 設定，不是寫死。
- 第一版**不做**分散式計數器（規格 §82 禁止 Redis Cluster）。單機做對，測到瓶頸再談。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §30（限制種類）、§31（soft／hard）、§42（估算 vs 實際）、§54（政策後台）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. 配額視窗 schema ＋ 計數寫入路徑。
2. soft 降級階梯 ＋ hard 拒絕 ＋ 兩個陰性對照。
3. 對帳：provider-reported 回來後修正估算。
