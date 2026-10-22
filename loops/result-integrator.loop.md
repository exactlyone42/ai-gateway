# LOOP: result-integrator

> 主體上限 **200 行**（不含「已做」）、**實時更新**。方法見全域 `start-work` skill。
> 卡 #待建立｜分支 `feature/result-integrator`｜段落 `S15`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P0 |
| 前置 | `task-decomposer`、`model-dispatcher` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
用另行分派的整合模型與程式檢查，把子結果組成唯一 `CanonicalResponse`，再正確回到原 Client 協定。

## 完成判準
- [ ] 整合前檢查 orchestration ID、task coverage、重複／未知結果、必要缺口與 context 上限。
- [ ] 整合模型走分派器的完整硬條件，角色為 integrator；Jev 只排序合格候選。
- [ ] 整合後檢查必要 task 引用、缺口、矛盾揭露、輸出上限與 structured-output schema。
- [ ] Anthropic Messages、OpenAI Responses／Chat 與自家測試 Client 各能得到合規單一回應；flag 關閉時原 native 路徑不變。
- [ ] 工具互動與不支援欄位 bypass；取消傳到整張圖；最終串流符合原入口錯誤格式。
- [ ] 含歷史 ToolCall／ToolResult 的 Client 續輪也 bypass；內部等待期間 heartbeat 不製造假的語意事件。
- [ ] wire 只顯示公開模型別名及 aggregate usage；內部模型明細只在授權 Admin 查詢。
- [ ] tenant mode 預設 disabled，shadow/enforced 有 CAS、RBAC、audit 與一鍵回退。
- [ ] 正式啟用前有單模型對照的成功率、品質、p50/p95 延遲、Provider/Jev 成本與部分失敗率；未授權不得實送付費流量。
- [ ] `./scripts/check.sh all` 通過，三種 Client 的固定案例與機密掃描全綠。
- [ ] 陰性對照：子結果注入指令、隱藏矛盾、漏必要 task、schema 錯誤、local-only 選線上時，整合或政策檢查必須變紅且不回假成功。

## 已知
- 既有 `CanonicalResponse` 與 response/stream adapters 可把單一結果轉回來源協定。
- 整合是新的 Provider 呼叫，必須由 dispatcher 選模型並獨立記帳，不能由 Client 或 Jev 私下完成。
- 詳細工作切片在 `docs/superpowers/plans/2026-10-22-result-integrator.md`。

## 已做
- `2026-10-22` 已完成共用架構規格與詳細實作計畫；尚未實作或實測。

## 已確定不可行
- 由原 Client 整合：會讓 Claude Code、Codex 與自家 Client 得到不同核心能力。
- 只相信整合模型自稱完整：無法機械發現漏項、矛盾與 schema 違規。

## 證據
- `src/canonical.rs` 已定義 `CanonicalResponse` 與 usage；證明有統一出口型別，不證明多結果整合正確。
- `src/adapters/response.rs`、`src/adapters/stream.rs` 是既有協定出口；證明可復用轉換，不證明編排期間的延遲符合 Client 容忍度。

## 來源
- `docs/multi-model-orchestration.md`；本 repo 原始碼，查證日 2026-10-22。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 整合者 | 另選 integrator model | Client 或固定單一模型 | 跨 Client 一致且受相同政策 |
| 串流 | 只串最終整合輸出 | 串流內部子結果 | 不洩漏中間內容或破壞協定 |
| 工具 | 第一版 bypass | 隱藏執行 Client 工具 | Gateway 無權替 Client 執行本地工具 |

## 下一步
1. 等前兩個 LOOP 完成，再依 PLAN 建前置檢查、integrator selection、後置驗證與 coordinator。
2. 本機與 shadow 證據完成後，另取得部署與付費實送授權才做正式啟用驗收。
