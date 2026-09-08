# LOOP: providers-expansion

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 待開（開工時建）｜分支 `feature/providers-expansion`｜段落 `W16`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P2 |
| 前置 | `canonical-layer` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
加一個 Provider 不需要改 Core。

## 完成判準
- [ ] `GeminiProvider`／`XAIProvider`／`LocalProvider`（vLLM、Ollama）都實作同一組介面：
      `capabilities()`／`health()`／`send()`／`stream()`／`estimate_cost()`。
- [ ] **驗收即設計檢查**：新增這三個 Provider 對 `gateway-core` 的 diff **必須為 0 行**。
      不為 0 就代表 Adapter 抽象設計錯了 → 退回 W14 修，不是在這裡繞過。
- [ ] 每個 Provider 一組 contract test（含串流、錯誤、用量欄位對映）。
- [ ] 每個 Provider 的 token 定義差異寫進 `docs/provider-protocols.md`（不假設可直接相加）。

## 已知
- 規格 §73 就是拿「要不要改 Core」當抽象品質的檢驗標準。
- Local LLM（vLLM／Ollama）通常是 OpenAI 相容協定，但 usage 欄位與能力回報常有缺漏——
  這正好是驗證「能力不匹配不得 silent drop」的好案例。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填：`git diff --stat crates/gateway-core` 必須是空的）

## 來源
- 規格 §19（Provider 介面）、§73（Phase 6）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. 先做 `LocalProvider`（本機可跑、不花錢、迭代最快）。
2. 再做 Gemini、xAI。
3. 每加一個就檢查 `gateway-core` 的 diff 是否為 0。
