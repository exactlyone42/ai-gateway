# AI 平台：全案決策版

2026-10-24｜沿用原工作區與原 LOOP｜重大方向均未定案

**這一版可供決策，整體產品仍未完成。** 目標是讓自己的介面收任務、讀必要資料、使用模型與工具，並能看結果及停止。Gateway 繼續提供模型入口。

## 現況：已有零件，尚未接成自家產品

<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 540" style="width:100%;font-family:Noto Sans CJK TC;color:#173c47"><defs><marker id="arrow" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M0 0L8 4L0 8" fill="#257b75"/></marker></defs><rect x="10" y="10" width="220" height="80" rx="10" fill="#edf5f4" stroke="#72918e"/><text x="120.0" y="39" text-anchor="middle" font-size="17" font-weight="bold">既有 Client</text><text x="120.0" y="63" text-anchor="middle" font-size="13">由原工具處理對話及工具</text><rect x="315" y="10" width="190" height="80" rx="10" fill="#edf5f4" stroke="#72918e"/><text x="410.0" y="39" text-anchor="middle" font-size="17" font-weight="bold">Gateway</text><text x="410.0" y="63" text-anchor="middle" font-size="13">保留模型入口</text><rect x="590" y="10" width="200" height="80" rx="10" fill="#edf5f4" stroke="#72918e"/><text x="690.0" y="39" text-anchor="middle" font-size="17" font-weight="bold">模型供應來源</text><text x="690.0" y="63" text-anchor="middle" font-size="13">正式全路徑本輪未重測</text><rect x="10" y="230" width="240" height="85" rx="10" fill="#edf5f4" stroke="#72918e"/><text x="130.0" y="259" text-anchor="middle" font-size="17" font-weight="bold">獨立本地測試程式</text><text x="130.0" y="283" text-anchor="middle" font-size="13">不是自家產品介面</text><rect x="350" y="230" width="200" height="85" rx="10" fill="#edf5f4" stroke="#72918e"/><text x="450.0" y="259" text-anchor="middle" font-size="17" font-weight="bold">Ollama</text><text x="450.0" y="283" text-anchor="middle" font-size="13">本地模型服務</text><rect x="590" y="230" width="200" height="85" rx="10" fill="#fff2d9" stroke="#72918e"/><text x="690.0" y="259" text-anchor="middle" font-size="17" font-weight="bold">模型檔案</text><text x="690.0" y="283" text-anchor="middle" font-size="13">實際版本見原始輸出</text><rect x="10" y="410" width="240" height="80" rx="10" fill="#fff2d9" stroke="#72918e"/><text x="130.0" y="439" text-anchor="middle" font-size="17" font-weight="bold">測試文件與結果</text><text x="130.0" y="463" text-anchor="middle" font-size="13">本地證據目錄</text><rect x="350" y="410" width="200" height="80" rx="10" fill="#edf5f4" stroke="#72918e"/><text x="450.0" y="439" text-anchor="middle" font-size="17" font-weight="bold">獨立取消對照</text><text x="450.0" y="463" text-anchor="middle" font-size="13">容器可停；尚未整合</text><path d="M230,32 L315,32" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="240" y="25" font-size="12">模型請求</text><path d="M315,70 L230,70" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="253" y="86" font-size="12">回覆</text><path d="M505,32 L590,32" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="531" y="25" font-size="12">轉送</text><path d="M590,70 L505,70" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="531" y="86" font-size="12">回覆</text><path d="M250,251 L350,251" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="251" y="240" font-size="12">模型／工具描述</text><path d="M350,293 L250,293" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="251" y="310" font-size="12">工具要求／答案</text><path d="M590,270 L550,270" fill="none" stroke="#ac7227" stroke-width="2"  stroke-dasharray="5 4" marker-end="url(#arrow)"/><text x="555" y="258" font-size="12">載入</text><path d="M100,315 L100,410" fill="none" stroke="#ac7227" stroke-width="2"  stroke-dasharray="5 4" marker-end="url(#arrow)"/><text x="112" y="365" font-size="12">讀檔／保存</text><text x="12" y="530" font-size="13">實線：請求與回覆　　虛線：資料存取或尚未採用的連接</text></svg>

本圖區分現有入口與獨立測試，沒有把小測試畫成已整合。正式模型路徑依原始碼整理，本輪未重送正式 Client；本地模型往返有原始輸出。

證據：原工作區 `src/adapters/provider.rs`、`src/services/openai.rs`；`docs/evidence/agent-platform/followup/model-tool-results.json`。現況快照是當時觀測，不保證服務現在仍為相同版本。


<div style="break-after:page"></div>

# 目標：各區塊怎麼連接

**示意目標，全部不表示已部署。** 先決定任務由哪一層管理，再選底座。把圖上的任務程式放在 Gateway 裡，與獨立服務的改動範圍不同，不能默認任何一種。

<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 540" style="width:100%;font-family:Noto Sans CJK TC;color:#173c47"><defs><marker id="arrow" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M0 0L8 4L0 8" fill="#257b75"/></marker></defs><rect x="10" y="10" width="210" height="80" rx="10" fill="#edf5f4" stroke="#72918e"/><text x="115.0" y="39" text-anchor="middle" font-size="17" font-weight="bold">自家操作介面</text><text x="115.0" y="63" text-anchor="middle" font-size="13">送任務、看結果、核准、停止</text><rect x="290" y="10" width="220" height="80" rx="10" fill="#edf5f4" stroke="#72918e"/><text x="400.0" y="39" text-anchor="middle" font-size="17" font-weight="bold">任務程式：待定案</text><text x="400.0" y="63" text-anchor="middle" font-size="13">可獨立放置／可放 Gateway</text><rect x="10" y="190" width="220" height="85" rx="10" fill="#fff2d9" stroke="#72918e"/><text x="120.0" y="219" text-anchor="middle" font-size="17" font-weight="bold">對話與來源資料</text><text x="120.0" y="243" text-anchor="middle" font-size="13">只保存使用者同意的資料</text><rect x="290" y="190" width="220" height="85" rx="10" fill="#edf5f4" stroke="#72918e"/><text x="400.0" y="219" text-anchor="middle" font-size="17" font-weight="bold">工具執行區</text><text x="400.0" y="243" text-anchor="middle" font-size="13">按任務隔離、限制可碰資料</text><rect x="590" y="10" width="200" height="80" rx="10" fill="#edf5f4" stroke="#72918e"/><text x="690.0" y="39" text-anchor="middle" font-size="17" font-weight="bold">Gateway</text><text x="690.0" y="63" text-anchor="middle" font-size="13">模型入口、權限與用量</text><rect x="590" y="190" width="200" height="85" rx="10" fill="#edf5f4" stroke="#72918e"/><text x="690.0" y="219" text-anchor="middle" font-size="17" font-weight="bold">本地或官方 API</text><text x="690.0" y="243" text-anchor="middle" font-size="13">模型回傳文字或工具要求</text><rect x="290" y="390" width="220" height="85" rx="10" fill="#edf5f4" stroke="#72918e"/><text x="400.0" y="419" text-anchor="middle" font-size="17" font-weight="bold">檔案／網頁／桌面</text><text x="400.0" y="443" text-anchor="middle" font-size="13">有界操作；寫入依原授權</text><rect x="590" y="390" width="200" height="85" rx="10" fill="#fff2d9" stroke="#72918e"/><text x="690.0" y="419" text-anchor="middle" font-size="17" font-weight="bold">雲端 GPU：可選</text><text x="690.0" y="443" text-anchor="middle" font-size="13">不足才評估；目前不租用</text><path d="M220,32 L290,32" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="240" y="25" font-size="12">任務</text><path d="M290,70 L220,70" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="226" y="86" font-size="12">進度／結果</text><path d="M510,32 L590,32" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="520" y="25" font-size="12">模型請求</text><path d="M590,70 L510,70" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="520" y="86" font-size="12">模型回覆</text><path d="M650,90 L650,190" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="612" y="140" font-size="12">請求</text><path d="M730,190 L730,90" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="740" y="140" font-size="12">回覆</text><path d="M340,90 L120,190" fill="none" stroke="#ac7227" stroke-width="2"  stroke-dasharray="5 4" marker-end="url(#arrow)"/><text x="174" y="133" font-size="12">讀取／保存</text><path d="M420,90 L420,190" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="432" y="139" font-size="12">呼叫／取消</text><path d="M375,190 L375,90" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="315" y="162" font-size="12">工具結果</text><path d="M360,275 L360,390" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="320" y="332" font-size="12">操作</text><path d="M455,390 L455,275" fill="none" stroke="#257b75" stroke-width="2"  marker-end="url(#arrow)"/><text x="466" y="332" font-size="12">結果</text><path d="M690,390 L690,275" fill="none" stroke="#ac7227" stroke-width="2"  stroke-dasharray="5 4" marker-end="url(#arrow)"/><text x="700" y="334" font-size="12">選定後才接入</text><text x="12" y="530" font-size="13">實線：請求與回覆　　虛線：資料存取或尚未採用的連接</text></svg>

任務程式整理上下文，也就是本次給模型參考的資料；它接收模型提出的工具要求，核對權限，再交工具執行區。模型憑證留在服務端。結果回介面，必要的來源與對話保存在獲准的位置。

快取放在模型服務一側；先重用相同開頭的計算，不先重用可能過期的工具結果。資料搜尋與雲端算力都是按缺口增加，不是首版前置。


<div style="break-after:page"></div>

# 七個區塊：現況、選項與採用條件（1）

| 區塊 | 已有證據與缺口 | 候選、代價與驗收 |
| --- | --- | --- |
| Gateway | 原始碼有 Ollama 接法及快取用量欄位；這不是正式請求成功證據。自家介面尚未接入。 | 沿用 Rust Gateway，改動較少；替換核心需重驗相容性，本輪不建議。選定底座後，原 Client 與新介面各實送，確認模型、權限、錯誤及用量均正確。 |
| 上下文與資料品質 | 短摘要保留三項限制，刪掉限制的錯誤摘要會被拒；長對話、過期文件、來源衝突未測。 | 先用檔案搜尋與帶來源摘要；需要按意思搜尋時，才比較 PostgreSQL 全文搜尋與 pgvector。後者要維護文字轉數字的模型及索引。用相同問題測來源、版本、限制是否完整，不能只看回答流暢。 |
| 快取 | 本地工具往返第一筆回報 0、第二筆回報 334 個已快取輸入單位。不是正式帳單或控制變因實驗。 | 先保留供應商／本地服務的原生快取；vLLM 可重用相同開頭計算。自建答案快取會增加過期與資料隔離問題，暫不做。後續固定模型、題目與長度，分首次／重複請求，核對用量及耗時。 |
| 本地模型 | 既有 Ollama 上 gpt-oss 完成短題，Qwen 衍生模型完成一次讀檔往返；不代表官方 Qwen 品質。 | 先沿用 Ollama；llama.cpp 留為需調整硬體使用方式時的候選。換引擎有模型格式、工具回傳與記憶體代價。用完整任務比較正確率、時間及記憶體峰值，未測前不排名。 |

來源：本地 `local-measurements.json`、`followup/summary-checks.json`、`followup/model-tool-results.json`；[Ollama 工具呼叫](https://docs.ollama.com/capabilities/tool-calling)、[pgvector](https://github.com/pgvector/pgvector)、[PostgreSQL 全文搜尋](https://www.postgresql.org/docs/current/textsearch.html)、[vLLM 快取](https://docs.vllm.ai/en/stable/features/automatic_prefix_caching/)。

**新確認的範圍：** 334 是原始回覆提供的計數；沒有計算成金額，也不能用兩筆不同內容的耗時差，宣稱快取造成多少加速。


<div style="break-after:page"></div>

# 七個區塊：現況、選項與採用條件（2）

| 區塊 | 已有證據與缺口 | 候選、代價與驗收 |
| --- | --- | --- |
| 工具與電腦操作 | 檔案搜尋、模型讀檔往返有輸出。Codex 獨立命令取消後仍寫檔；容器對照可停，但未整合。網頁與桌面全流程未測。 | 檔案工具優先；網頁用 Playwright 依按鈕和欄位定位；只有畫面的程式再評估 UI-TARS。後者需處理畫面辨識錯誤。完整流程須驗無權限寫入被拒、取消後沒有後續動作；不要只看取消回覆。 |
| 自家操作介面 | 本機 Codex 0.163.0 產生的接入規格包含對話、核准與中斷；尚無本案可用介面。 | A：Codex app-server，沿用現有執行程式；B：Qwen Code，以本地路線優先；C：OpenHands，以遠端工作區優先。都要做自己的介面接法，不能把 CLI 當完成。驗送任務、結果、核准、拒絕、停止、續接。 |
| 雲端 GPU | 本案沒有租卡、速度或價格實測，現有本機測試也未證明必須租卡。 | 先維持不租；不足時比較官方 API 與自租 GPU 上 vLLM。租卡另有閒置、儲存和維運成本；API 要評估資料外送。先訂實際需求及預算，再由使用者授權，不能只比每小時價格。 |

來源：[Playwright 定位](https://playwright.dev/docs/locators)、[UI-TARS](https://github.com/bytedance/UI-TARS)、[Qwen Code](https://github.com/QwenLM/qwen-code)、[OpenHands SDK](https://github.com/OpenHands/software-agent-sdk)。Codex 接入能力依本機產生的 `codex-schema/v2/`；取消證據在 `cancellation/`。

## 建議與事實分開

建議先保留 Gateway，任務管理獨立放置，先做單人、檔案任務；這是縮小改動面的工程判斷，**尚未被選定**。它不等於選 A，也不等於批准 Codex 底座。

不增加多套模型引擎、永久記憶或雲端服務，能減少第一條流程的變數。若使用者優先要求本機資料或多人遠端，第一個底座與驗收順序就應改變。


<div style="break-after:page"></div>

# 重大方向：需要分開決定兩件事

## 1．任務管理放在哪裡

| 方向 | 好處 | 代價 |
| --- | --- | --- |
| 獨立任務程式（建議，未定案） | Gateway 保留模型入口；任務、資料與工具可獨立驗證 | 既有 Client 不會自動取得新任務功能，需要接自家介面 |
| 放在 Gateway | 從共用入口提供任務能力 | 需要處理不同 Client 的工具往返與原請求語意，影響範圍較大 |

## 2．第一版優先解決什麼

| 候選 | 節省與速度 | 安全與便利 | 目前門檻 |
| --- | --- | --- | --- |
| A 沿用 Codex 能力做介面 | 可少寫對話與工具管理；沒有完整工期證據 | 接入規格現成；要驗模型相容性與權限 | 取消整合及完整任務未過，不能直接採用 |
| B 資料優先留在本地 | 沿用 Ollama；不必預先換 llama.cpp | 資料流較可控，但仍需檢查工具是否連網 | Qwen Code 接入、品質與長任務未驗 |
| C 遠端工作區優先 | 可參考 OpenHands 組成；服務較多 | 適合需要遠端隔離的方向，維運較重 | 多人權限與遠端環境未驗；未授權部署 |

這是相對工程取捨，不是測得的速度或成本排名。沒有足夠證據宣稱任何候選同時最便宜、最快且最安全。

**請 cash 決定：任務管理的位置，以及第一版以 A／B／C 哪個優先。** 兩項可分開選；選定後仍須通過採用條件。未答覆前不視為選 A，不建立產品實作分支，不開始付費或正式部署。


<div style="break-after:page"></div>

# 實作順序與整體驗收

| 順序 | 交付 | 依賴／何時通過 |
| --- | --- | --- |
| 0 已完成本輪整理 | 七區現況、候選、兩張圖、證據邊界 | 本地原件與官方來源核對；不是產品完成 |
| 1 待 cash 決定 | 任務管理位置、首版優先方向 | 決定前不能把底座、UI 或 Gateway 改動當作已授權 |
| 2 選定後 | 一個任務在隔離測試環境跑到底 | 採用底座通過；包含核准、拒絕與停止。底座不合格則回到候選比較 |
| 3 接上介面 | 使用者從自家介面完成同一任務 | 能看到過程、結果、失敗；停止後不再操作，續接仍保留限制 |
| 4 同題比較 | 固定五個任務，記錄品質、時間、用量 | 不混用小測試當基準；實際完整流程取得可比資料 |
| 5 按瓶頸補強 | 搜尋、快取、本地模型、網頁或桌面能力 | 只做有測量支持的缺口；雲端另等費用授權 |

## 固定任務草案，可跨候選重用

1. 查出正式與開發設定，附檔案來源；錯埠答案必須失敗。
2. 找指定文件及不存在的文件；不能捏造來源。
3. 摘要保留禁止事項、下一步與原文位置；過期資料不得冒充最新。
4. 讀指定檔案，提出修改到測試副本；拒絕核准時不得寫入。
5. 啟動有延後動作的測試任務，從介面停止；未取消對照可完成，取消組不得留下後續動作。

既有五項小測試提供部分證據；以上完整流程均尚未驗收。下一輪不是重做取消追蹤，而是依決定接通第一條完整流程。

## 尚未做的檢查與界線

本輪只整理研究、讀原件與原始碼、查官方來源、生成並檢查 PDF；未跑 Rust 全套、正式 Client、完整網頁／桌面任務、長上下文或跨模型基準。未修改正式服務、未付費、未刪除、未提交。


<div style="break-after:page"></div>

# 證據索引與停止位置

原工作區：`/home/cash/.cache/agent-platform-decisions`。分支沿用 `feature/agent-platform-decisions`，沒有另開 LOOP。

| 證據 | 能證明什麼 | 不能證明什麼 |
| --- | --- | --- |
| `environment.json` | 2026-10-24 01:42 的模型及環境快照 | 現在仍是同一部署版本 |
| `local-measurements.json` | 短題輸出與錯埠對照 | 長任務正確率 |
| `followup/summary-checks.json` | 短摘要限制與錯誤摘要對照 | 長對話完整性 |
| `followup/model-tool-results.json` | 模型讀檔往返、兩筆快取計數 | Gateway 整合與正式費用 |
| `cancellation/tool-results.json` | 獨立命令取消後仍執行的案例 | 所有 Codex 功能都無法取消 |
| `cancellation/container-results.json` | 獨立容器可停止延後寫入 | 介面、任務程式與容器已整合 |
| `decision-review/` | 本輪查重、來源與文件驗證 | 全案已完成 |

以上相對路徑均在 `docs/evidence/agent-platform/` 下。本輪核對原始 JSON，不以舊報告敘述代替行為證據。

## 本輪查閱的官方資料

[Ollama 工具呼叫](https://docs.ollama.com/capabilities/tool-calling) 支持模型提出工具要求，再將工具結果送回模型的接法。

[Qwen Code](https://github.com/QwenLM/qwen-code) 與 [OpenHands SDK](https://github.com/OpenHands/software-agent-sdk) 用於核對候選定位；本機尚未測它們的完整流程。

[pgvector](https://github.com/pgvector/pgvector) 提供 PostgreSQL 的相似內容搜尋；[vLLM 快取](https://docs.vllm.ai/en/stable/features/automatic_prefix_caching/) 重用相同開頭的模型計算；兩者都不能直接推論本案收益。

[Playwright](https://playwright.dev/docs/locators) 支持依頁面結構定位；本案尚未測真實網頁任務。網頁資料查閱於 2026-10-24，未鎖定候選未來採用版本。

## 等待的必要決定

七區比較與依賴整理完成後，餘下產品實作依賴第 5 頁的兩項重大選擇。依使用者要求，在確認 PDF 可讀並保存證據後停用 `agent-platform-decisions` 排程，等待 cash 定案。整體狀態保持未完成，不自動喚醒。
