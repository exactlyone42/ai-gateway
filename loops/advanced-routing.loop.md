# LOOP: advanced-routing

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 #TBD｜分支 `feature/advanced-routing`｜段落 `W17`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P2 |
| 前置 | `providers-expansion` |
| 開始 | — |
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

## 已做
- （待填）

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
