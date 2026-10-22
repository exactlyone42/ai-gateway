# LOOP: task-decomposer

> 主體上限 **200 行**（不含「已做」）、**實時更新**。方法見全域 `start-work` skill。
> 卡 #待建立｜分支 `feature/task-decomposer`｜段落 `S13`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中（Task 1–5 完成，待真實模型品質與跨協定等價驗收） |
| 優先度 | P0 |
| 前置 | `canonical-layer`、`model-task-classification` |
| 開始 | 2026-10-22 |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
在 `CanonicalRequest` 上把複合要求拆成完整、無重複且不改變意圖的有限任務圖；簡單或不安全請求維持單模型。

## 完成判準
- [x] 共用 `TaskGraph` 契約、8 節點／2 層上限、唯一 ID、依賴存在、無循環與資料政策不可降級都有機器測試。
- [ ] 至少 30 個複合案例的必要子任務召回率 ≥90%，新增或改變意圖為 0；至少 10 個簡單／工具案例 100% `Direct`。
- [ ] Anthropic Messages、OpenAI Responses／Chat 的同義 canonical 輸入得到等價 decision。
- [x] `DataPolicy` 三值、繼承順序與 canonical 能力判斷有測試；含歷史 ToolCall／ToolResult 的續輪請求必須 `Direct`。
- [x] shadow 不改路由與 Provider 呼叫數，只存 mode、reason、node_count、depth、elapsed、version。
- [x] tenant 預設 disabled；shadow 拆解呼叫也受硬政策限制，LocalOnly 只用本地、不呼叫 Jev，無本地模型回 `Direct`。
- [x] 逾時、低信心、壞 JSON、越界圖、Client 工具與不支援欄位回原單模型路徑。
- [ ] 機密掃描涵蓋拆解輸入與輸出；prompt／objective 不進 log、trace、ledger、spool。
- [ ] `./scripts/check.sh all` 通過，文件與報表同步。
- [ ] 陰性對照：注入循環、9 節點、三層依賴、重複 ID、政策降級或 prompt sentinel，圖驗證或機密檢查必須變紅，且 Provider 工作呼叫為 0。

## 已知
- `src/adapters/protocol.rs` 已把各入口投影成 `CanonicalRequest`，拆解器不得重新解析 Client JSON。
- `src/task_classification.rs` 目前只選八類之一；遇多意圖會回無分類，不能取代任務拆解。
- 詳細工作切片在 `docs/superpowers/plans/2026-10-22-task-decomposer.md`。

## 已做
- `2026-10-22` 已完成共用架構規格與詳細實作計畫。
- `2026-10-22` 實作 Task 1–5（commit da25f2f、868e39a、33135a5 與 shadow 拆解器）：契約、資料政策、30＋8＋2 固定案例評分器、有界拆解器（5 秒逾時、64KiB、信心 0.7）、migration 三表、shadow 接入與管理報表。`check.sh gate` 通過；全套 lib 279 過 1 敗（既有失敗）、其餘整合測試全過。陰性對照：洩漏 prompt 進日誌使 sentinel 測試變紅；`mode == "shadow" || true` 使預設 disabled 測試變紅，皆已還原。

## 已確定不可行
- 由 Claude Code 或 Codex 自行拆解：其他 Client 行為不一致，核心能力綁定入口。
- 直接把現有單一 task classifier 當拆解器：它不產生依賴圖，也無法保存多項要求。

## 證據
- `src/canonical.rs` 的 `CanonicalRequest` 是已正規化請求；證明可作統一輸入，不證明拆解品質。
- `src/adapters/protocol.rs` 的 decode/enrich 建立 canonical view；證明入口已收斂，不證明所有原生欄位都可多模型化。

## 來源
- `docs/multi-model-orchestration.md`；本 repo 原始碼，查證日 2026-10-22。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 首次上線 | shadow | 直接執行子任務 | 先量錯拆與延遲，不改正式答案 |
| 不安全請求 | `Direct` | 嘗試轉換工具迴圈 | 不破壞 Client 協定 |
| 品質評分 | 固定人工標記案例 | 模型自評 | 可重播且有陰性對照 |

## 下一步
1. 補三協定同義輸入的 decision 等價測試與機密掃描（拆解輸入／輸出）。
2. 用自然流量的 shadow 報表 `/admin/v1/task-decomposition-shadow` 量真實模型的召回與錯拆；固定案例只驗評分器與契約，不代表正式品質。
3. `check.sh all` 目前只被既有的 `identity::tests::lifecycle_is_tenant_safe_and_revocation_beats_the_cache` 擋住（基準 33135a5 單獨跑也失敗，502≠400），需另案處理。
4. 以上完成後才凍結契約，讓 `model-dispatcher` 開始實作。
