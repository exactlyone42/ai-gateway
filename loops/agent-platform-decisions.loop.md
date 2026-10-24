# LOOP: agent-platform-decisions

> 分支 `feature/agent-platform-decisions`｜研究與證據，不是 Gateway 實作

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中（計畫已交付，產品實作待做） |
| 優先度 | P1 |
| 前置 | 無 |
| 開始 | `2026-10-22` |
| 結束 | 未結束 |

## 目的

從現有 Gateway 走到可實際工作的 AI 平台：自己的操作介面能接收任務、掌握對話與必要資料，使用本地或遠端模型，呼叫工具操作檔案、網頁與電腦；Gateway 繼續提供模型服務入口。算力不足時再評估雲端 GPU。

本階段交付現況與目標分區架構圖，各區塊列出技術選項與來源，再按依賴安排實作順序，讓 cash 決定重大方向後逐塊實作。

## 完成判準

- [x] A1 Gateway、上下文、快取、本地模型、工具、自家 Client、雲端 GPU 各有證據、候選、代價與未驗證事項。
- [ ] A2 使用者確認重要方向，逐項定案後才實作；以真實流程驗收。
- [x] A3 PDF 與進度同步，區分已實測、未測、已部署。
- [ ] 已定案的第一個完整任務能從操作介面送出，經模型與工具完成，看到結果並能停止；後續以相同任務比較品質、時間與用量。
- [ ] 陰性對照：把未定案的方案寫成已選定，進度表與圖的檢查必須變紅。小測試完成只代表取得證據，整體目標仍未完成。

## 已知

- 2026-10-24 使用者要求 Agent 獨立 repo、Tauri 桌面 Client 與可長任務執行的計畫。舊「任務放 Gateway／預設 Web」不再當未決方向。
- 新 repo：`/home/cash/work_space/private/agent-platform`，https://github.com/DnaBoss/agent-platform。PLAN.md 列 10 步、技術、交付、驗收、錯誤注入與 Evaluation，並補 14 個開源來源。
- P01 至 P10 均 PENDING。Rust 全套、Tauri 實機、模型完整任務、RAG、Memory、Computer Use、故障與 Evaluation 未跑。
- 舊 followup／cancellation 只是局部證據，不能當新產品已整合。
- 正式環境未動，未花費、部署或刪除。原排程 `ad8983b6-1875-4762-8031-1a12b989a6aa` 讀回 enabled=false，沒有宣稱產品正在背景開發。

## 已做

| 步驟 | 成果 | 狀態 |
| --- | --- | --- |
| S01 | 查證現況、識別決策衝突、取得初步實測 | DONE |
| S02 | 使用者確定 Agent 獨立 repo 與桌面 Client | DONE |
| S03 | 完成不依賴架構選擇的五項小測試 | DONE |
| S05 | 定位取消失敗並驗證獨立容器停止工作 | DONE |
| S06 | 七區決策圖、選項與依賴；本地研究交付 | DONE |
| S04 | PLAN 已交付；P01 至 P10 待實作與產品驗收 | READY |

## 已確定不可行

- 舊 `feature/multi-model-orchestration-plan` 在 Gateway 拆解任務，與 PDF 邊界不同，不可直接接手其實作。
- 不再擴做單一取消問題；它只是局部證據。

## 證據

- 計畫品質：新 repo `docs/evidence/plan-validation.json`；格式、13 個內部連結、架構圖、22 項共用 policy 測試通過。證明計畫文件可用，證明不了產品可用。
- 指定 PDF：`/home/cash/Documents/高質量文件/agent-platform-技術棧決策.pdf`。
- checkpoint 第 21 筆 RUNNING／READY 表示可接續，不代表背景程序已啟動。

## 來源

- 本地 loops、分支、開啟的 issue／PR 已查，無同名全案；既有 #20 是上層工程議題，不另建外部卡。
- 研究資料與來源清單在 `docs/agent-platform-decision-progress.md` 與 `docs/evidence/`。

## 下一步

1. 依新 repo `docs/longrun.md` 查重，從 P01 續做：比較 Codex 現成 Rust Harness 與 Rig。
2. 使用者確認重要方向（A2）後才實作。
