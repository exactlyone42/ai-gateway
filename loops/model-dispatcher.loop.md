# LOOP: model-dispatcher

> 主體上限 **200 行**（不含「已做」）、**實時更新**。方法見全域 `start-work` skill。
> 卡 #待建立｜分支 `feature/model-dispatcher`｜段落 `S14`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P0 |
| 前置 | `task-decomposer` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
依硬政策與能力執行已驗證任務圖，讓各子任務選到合格模型並回傳可追溯結果；Jev 只排序候選。

## 完成判準
- [ ] 任務狀態機、依賴、部分失敗與 aggregate usage 有純單元測試。
- [ ] 每節點先過資料政策、能力、tenant policy、quota、健康及 scheduler admission，再交 task profile／Jev 排序。
- [ ] 最多 3 並行、每節點最多一次 Provider 呼叫；失敗不隱藏重試。
- [ ] `local_only` 無本地候選時零 Provider 呼叫並回穩定錯誤，不退回線上。
- [ ] `local_only` 內容不呼叫外部 Jev；非 local-only 也只有明確允許內容外送才呼叫。
- [ ] 取消停止未開始工作、傳到進行中連線並保留已完成結果；unknown 不因重啟重送。
- [ ] 整張圖沿用 request deadline；stream 在拆解、worker 與整合等待期間持續送既有 SSE comment heartbeat。
- [ ] 父、拆解、子任務與整合角色以含 execution_role 的唯一鍵逐筆對回；跨 tenant 隔離，內容不落庫。
- [ ] development 測試入口可執行固定圖；正式 Client 路徑仍關閉。
- [ ] `./scripts/check.sh all` 通過；取得授權才跑線上付費案例。
- [ ] 陰性對照：讓 Jev 回不合格模型、只供應線上候選給 local-only、製造取消競爭與失敗重入，選模／呼叫計數／帳務測試必須變紅。

## 已知
- `src/services/request_quota.rs` 已有候選過濾、task shortlist、Jev 與 admission 流程；應抽共用能力，不複製政策。
- `src/jev.rs` 目前只重排傳入候選，失敗保留原順序；此邊界可沿用。
- 詳細工作切片在 `docs/superpowers/plans/2026-10-22-model-dispatcher.md`。

## 已做
- `2026-10-22` 已完成共用架構規格與詳細實作計畫；尚未實作或實測。

## 已確定不可行
- Jev 直接從全部模型選：會繞過隱私、能力、預算與健康硬條件。
- 子任務自行重試：Provider 呼叫與成本不可預測，取消與帳務會失真。

## 證據
- `src/jev.rs` 的 `select` 只重排呼叫端提供的 `ModelSelection`；證明適合作建議排序，不證明候選本身合格。
- `src/services/request_quota.rs` 已在 Jev 前做能力、翻譯與政策過濾；證明可復用既有順序，不證明多節點併發安全。

## 來源
- `docs/multi-model-orchestration.md`；本 repo 原始碼，查證日 2026-10-22。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 並行 | 固定上限 3 | 依節點數全開 | 控制瞬間成本與資源 |
| 失敗 | 保留部分成功 | 任一失敗全丟 | 整合器仍可揭露可用結果 |
| 第一入口 | development 測試路由 | 直接接正式 Client | 分派可獨立驗收且不改使用者答案 |

## 下一步
1. 等 `task-decomposer` 契約凍結後，依 PLAN 建狀態機、候選選擇、執行器、取消、帳務與測試入口。
2. 完成固定圖實測與全套檢查後，才交給 `result-integrator` 串正式資料面。
