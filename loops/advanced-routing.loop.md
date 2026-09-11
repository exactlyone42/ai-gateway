# LOOP: advanced-routing

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#48](https://github.com/DnaBoss/agent-pool-gateway/issues/48)（健康路由／fallback 原在作廢的 [#6](https://github.com/DnaBoss/agent-pool-gateway/issues/6)）｜分支 `feature/advanced-routing`｜段落 `W17`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P2 |
| 前置 | `providers-expansion` |
| 開始 | `2026-09-11T11:32:12+08:00` |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
Provider 變慢或壞掉時自動換路，而且換路的理由與代價查得到。

## 完成判準
- [ ] Retry（同 Provider／模型重試）與 Fallback（換 Provider／模型）**遙測分開**，
      可以查到「請求 R1：嘗試 1 Anthropic timeout → 嘗試 2 OpenAI success」。
- [ ] 只對明確可重試的錯誤重試：transport failure／transient timeout／指定的 5xx／overload。
- [ ] **不重試**的情況要有測試守住：已開始串流、已產生 token 成本、工具有副作用、
      非幂等操作且結果不明。
- [ ] 健康感知／負載感知／成本感知／配額感知路由，各自可獨立開關。
- [ ] 自動品質路由（最後才做）。
- [ ] **陰性對照**：讓 mock provider 在串流開始後才失敗 → 系統**不得**重試（測試守住）。

## 已知
- 規格 §74 明確：這些是最後才加的，不要一開始做 ML Router。
- 規格 §41 要求 Retry 與 Fallback 的遙測分開，否則事後分不清是誰的問題。
- 規格 §40 列出重試的四個危險點，每一個都要有對應測試。
- 查重：2026-09-11 開工時已有本 loop，但沒有同題 open issue／PR、遠端 feature branch或
  其它 worktree owner；建立唯一卡 #48 與 `feature/advanced-routing`。
- Reviewer 使用獨立 `codex exec`；attempt／retry 邊界定案前與完整 diff 各審一次，高／中風險
  必須修正或以既有規格與負面對照裁定。
- 每段先跑精確測試，PR 前以隔離 DB 跑 `./scripts/check.sh all`。AI 可自動合入 `dev`；只做
  安全性、W17 必需依賴與 W17 目標，不順手做 W18 load harness 或 W19 lifecycle。

## 已做
- 2026-09-11T11:32+08:00：確認 W16 PR #47 已合入 `dev`（`2d6239b9`），關閉 #46；完成
  W17 issue／PR／branch 查重，建立卡 #48、Project #2 item 與唯一 feature branch。

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §40（retry）、§41（fallback）、§74（Phase 7）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. 先把 attempt 模型與遙測分開（不加任何自動決策）。
2. 再加健康感知，一次只加一個維度，每加一個量一次。
