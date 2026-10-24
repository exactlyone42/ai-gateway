# Agent Platform 技術棧決策

## GOAL：從現有 Gateway 走到可實際工作的 AI 平台

讓自己的操作介面能接收任務、掌握對話與必要資料，使用本地或遠端模型，呼叫工具操作檔案、網頁與電腦；現有 Gateway 繼續提供模型服務入口。算力不足時再評估雲端 GPU。

本階段要交付現況與目標分區架構圖。各區塊列出技術選項與來源，再按依賴安排實作順序。比較節省、高效、安全、便利與速度，整理幾個兼顧多項優點的方案，讓 cash 決定重大方向，再逐塊實作與驗收。

### 完成條件

- 範圍包含 Gateway、上下文與資料品質、快取、本地模型。也包含工具與電腦操作、自家操作介面、雲端算力。每塊各有現況、缺口、候選、取捨與驗收方式。
- 圖與進度表能看出模組之間怎麼連、先做什麼、依賴什麼，以及哪些決定尚未定案。
- 已定案的第一個完整任務能從操作介面送出，經模型與工具完成，看到結果並能停止；後續以相同任務比較品質、時間與用量。
- 小測試完成只代表取得證據；整體目標仍未完成。未定案的方案不能寫成已選定。

### 目前位置與下一步

2026-10-24後續使用者已要求Agent獨立repo、Tauri桌面Client，以及可長任務執行的計畫。舊「任務放Gateway／預設Web」不再當未決方向。新repo：/home/cash/work_space/private/agent-platform；PLAN.md已列10步、技術、交付、驗收、錯誤注入與Evaluation，並補14個開源來源（含大公司Harness）。已推送私人GitHub並在Orca開啟。

本輪交付計畫，未啟動產品實作；下一步P01先比較Codex現成Rust Harness與Rig。全案產品未完成，不把規劃驗收當成產品驗收。

## 整體驗收
- [x] A1 Gateway、上下文、快取、本地模型、工具、自家 Client、雲端 GPU 各有證據、候選、代價與未驗證事項。
- [ ] A2 使用者確認重要方向，逐項定案後才實作；以真實流程驗收。
- [x] A3 PDF 與進度同步，區分已實測、未測、已部署。

## 已知與來源
- 指定 PDF：/home/cash/Documents/高質量文件/agent-platform-技術棧決策.pdf
- 舊 feature/multi-model-orchestration-plan 在 Gateway 拆解任務，與 PDF 邊界不同；不可直接接手其實作。
- 查過本地 loops、分支、開啟的 issue／PR，無同名全案；既有 #20 是上層工程議題，不另建外部卡。
- 工作區：/home/cash/.cache/agent-platform-decisions；分支 feature/agent-platform-decisions，基於 dev 5da55ba。

## 計畫
| 步驟 | 成果 | 狀態 |
| --- | --- | --- |
| S01 | 查證現況、識別決策衝突、取得初步實測 | DONE |
| S02 | 使用者確定Agent獨立repo與桌面Client | DONE |
| S03 | 完成不依賴架構選擇的五項小測試 | DONE |
| S05 | 定位取消失敗並驗證獨立容器停止工作 | DONE |
| S06 | 七區決策圖、選項與依賴；本地研究交付 | DONE |
| S04 | PLAN已交付；P01至P10待實作與產品驗收 | READY |

## 目前狀態
- 計畫交付已完成，實作READY；全案未完成。checkpoint第21筆RUNNING/READY表示可接續，不代表背景程序已啟動。
- 新repo：https://github.com/DnaBoss/agent-platform；原研究工作區、分支、PDF與證據全部保留，未另建LOOP。
- 計畫與品質證據：新repo docs/evidence/plan-validation.json；格式、13個內部連結、架構圖、22項共用policy測試通過。
- P01至P10均PENDING。Rust全套、Tauri實機、模型完整任務、RAG、Memory、Computer Use、故障與Evaluation未跑。
- 舊followup/cancellation只是局部證據，不能當新產品已整合；不再擴做單一取消問題。
- 正式環境未動，未花費、部署或刪除。新repo已提交推送；原研究資料仍未提交。
- 原排程agent-platform-decisions（ad8983b6-1875-4762-8031-1a12b989a6aa）本輪再次讀回enabled=false，未建立替代排程。本輪範圍為使用者最新要求的計畫交付；沒有宣稱產品正在背景開發。
- 下一執行者依新repo docs/longrun.md查重，從P01續做；沿用本LOOP及原證據。
