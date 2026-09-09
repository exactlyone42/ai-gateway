# LOOP: test-harness-load

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 待開（開工時建；部分覆蓋於 [#19](https://github.com/DnaBoss/agent-pool-gateway/issues/19)）｜分支 `feature/test-harness-load`｜段落 `W18`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P1 |
| 前置 | 階段一：`openai-native`；階段二：`scheduler-admission` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
拿數字證明「不互串、不漏、不爆、不餓死」，不是靠綠燈。

## 完成判準（階段一，W06 之後）
- [ ] Mock provider 可控：SSE 節奏、中途中斷、429、500、malformed、partial、tool call、逾時。
- [ ] 100 條同時串流：回應不互串、記憶體不持續成長、FD 不洩漏、取消正常、
      Provider 併發不超限。
- [ ] 故障注入七項：Provider timeout／429／500／串流中斷／PostgreSQL 不可用／
      OTel 不可用／Client 斷線。每項都對照設計上「應該發生什麼」。
- [ ] 閘道自身 overhead 基準：P50／P95／P99、TTFT overhead、每條串流的記憶體、
      N 條串流的 CPU。**數字進 journal，不留「大概」**。

## 完成判準（階段二，W13 之後）
- [ ] 混合負載：10 互動 premium ＋ 30 互動 economy ＋ 30 背景 premium ＋ 30 批次 economy。
      驗證：互動優先、premium 保留槽生效、背景不霸佔容量、背景最後仍被服務、
      deadline 過期會取消、佇列滿正確拒絕。
- [ ] 每個結論都要有「關掉該機制就會變紅」的對照組，否則不算驗到。

## 已知
- 現況只有 1 個單元測試（`src/auth.rs:33` 常數時間比對）。沒有整合測試、沒有 mock provider。
- 全域規約：**任務核心目標未經實測驗證前不寫測試**。所以這一段排在 W06 之後，
  不在 W02～W05 期間先磨測試。
- 壓測的本業是量吞吐，不是寫單元測試（全域規約）。
- 參考做法：`/home/cash/work_space/private/essence/docs/LOADTEST_JOURNAL.md` 的紀錄格式。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填：每個數字附量測方法、環境、日期）

## 來源
- 規格 §75（測試種類）、§76（併發測試）、§77（排程測試）、§78（故障測試）、§79（效能測試）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. Mock provider 先做（W05 就會用到它）。
2. 階段一四項，數字進 `docs/loadtest-journal.md`。
3. 等 W13 完成再做階段二。
