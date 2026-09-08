# LOOP: scheduler-admission

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 #TBD｜分支 `feature/scheduler-admission`｜段落 `W13`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P1 |
| 前置 | `policy-engine`, `quota-ratelimit` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
100 個請求同時進來時，互動式工作不會被背景工作餓死，背景工作也不會永遠排不到。

## 完成判準
- [ ] Admission control 在進佇列前判斷七件事：憑證有效／配額／Provider 容量／佇列是否已滿／
      deadline 是否已過期／模型是否允許／預算是否允許。
      決策五種：accept／queue／downgrade／route elsewhere／reject。
- [ ] 佇列**有上限**、有 timeout、有 deadline、有明確的拒絕策略。
- [ ] Priority class ＋ 加權公平佇列 ＋ **aging**（避免背景永久 starvation）。
      權重起始值：critical 20／interactive 10／foreground 6／automation 3／background 2／batch 1。
- [ ] 保留容量：例如 premium 併發 20 = 互動保留 12 ／共用 4 ／背景 4。
      背景塞 1000 個請求**不得**吃掉互動的保留槽。
- [ ] 併發上限維度齊全：global／per-provider／per-model／per-user／per-device／per-client／
      per-agent／per-credential／per-workload。
- [ ] **禁止** `request → tokio::spawn() → 無上限上游請求`。
- [ ] Scheduler 只回答 WHEN；Router 回答 WHERE。兩者不同 module（驗收：程式碼審查 ＋ 依賴方向）。
- [ ] Scheduler 後台可見：Active／各 workload 排隊數／各品質等級已用槽位／Provider 併發／
      佇列 P50/P95／被拒數／過期數。
- [ ] **陰性對照 1**：關掉 aging → 背景工作在混合負載下必須被觀察到 starvation（測試變紅）。
- [ ] **陰性對照 2**：關掉保留容量 → 背景洪水必須把互動 TTFT 拉高（測試變紅）。
      兩條都是「證明這些機制真的在生效」，不是「有寫」。

## 已知
- 現況沒有排隊、沒有併發上限，請求直接轉發上游。
- 規格 §22 把排程器定位成核心能力：閘道不只是 Router，同時是 AI Compute Scheduler。
- 規格 §27 要求第一版就要有 aging，不能等。
- 規格 §28 把保留容量列為 P0/P1 重要功能。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填：混合負載實測數字，見 W18 階段二）

## 來源
- 規格 §22（排程器定位）、§27（加權 ＋ aging）、§28（保留容量）、§29（併發）、
  §32（admission）、§33（分離）、§55（排程後台）、§64（背壓）、§77（排程測試）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. Admission control 七項判斷 ＋ 五種決策。
2. 有上限佇列 ＋ WFQ ＋ aging。
3. 保留容量 ＋ 多維併發上限。
4. 兩個陰性對照（關掉機制要看得到症狀）。
