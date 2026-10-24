from pathlib import Path
import sys,json,hashlib
sys.path.insert(0,'/home/cash/tools')
import md_to_pdf
from playwright.sync_api import sync_playwright
root=Path('/home/cash/.cache/agent-platform-decisions')
out=root/'docs/evidence/agent-platform/decision-review'
base=Path('/home/cash/Documents/高質量文件/agent-platform-技術棧決策')
def box(x,y,w,h,title,sub,fill='#edf5f4'):
 return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="10" fill="{fill}" stroke="#72918e"/><text x="{x+w/2}" y="{y+29}" text-anchor="middle" font-size="17" font-weight="bold">{title}</text><text x="{x+w/2}" y="{y+53}" text-anchor="middle" font-size="13">{sub}</text>'
def line(x1,y1,x2,y2,label,lx,ly,dash=False):
 return f'<path d="M{x1},{y1} L{x2},{y2}" fill="none" stroke="{"#ac7227" if dash else "#257b75"}" stroke-width="2" {chr(32)+"stroke-dasharray=\"5 4\"" if dash else ""} marker-end="url(#arrow)"/><text x="{lx}" y="{ly}" font-size="12">{label}</text>'
def diagram(target):
 s='<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 540" style="width:100%;font-family:Noto Sans CJK TC;color:#173c47"><defs><marker id="arrow" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M0 0L8 4L0 8" fill="#257b75"/></marker></defs>'
 if target:
  s+=box(10,10,210,80,'自家操作介面','送任務、看結果、核准、停止')+box(290,10,220,80,'任務程式：待定案','可獨立放置／可放 Gateway')
  s+=box(10,190,220,85,'對話與來源資料','只保存使用者同意的資料','#fff2d9')+box(290,190,220,85,'工具執行區','按任務隔離、限制可碰資料')
  s+=box(590,10,200,80,'Gateway','模型入口、權限與用量')+box(590,190,200,85,'本地或官方 API','模型回傳文字或工具要求')
  s+=box(290,390,220,85,'檔案／網頁／桌面','有界操作；寫入依原授權')+box(590,390,200,85,'雲端 GPU：可選','不足才評估；目前不租用','#fff2d9')
  s+=line(220,32,290,32,'任務',240,25)+line(290,70,220,70,'進度／結果',226,86)
  s+=line(510,32,590,32,'模型請求',520,25)+line(590,70,510,70,'模型回覆',520,86)
  s+=line(650,90,650,190,'請求',612,140)+line(730,190,730,90,'回覆',740,140)
  s+=line(340,90,120,190,'讀取／保存',174,133,True)+line(420,90,420,190,'呼叫／取消',432,139)+line(375,190,375,90,'工具結果',315,162)
  s+=line(360,275,360,390,'操作',320,332)+line(455,390,455,275,'結果',466,332)
  s+=line(690,390,690,275,'選定後才接入',700,334,True)
 else:
  s+=box(10,10,220,80,'既有 Client','由原工具處理對話及工具')+box(315,10,190,80,'Gateway','保留模型入口')+box(590,10,200,80,'模型供應來源','正式全路徑本輪未重測')
  s+=box(10,230,240,85,'獨立本地測試程式','不是自家產品介面')+box(350,230,200,85,'Ollama','本地模型服務')+box(590,230,200,85,'模型檔案','實際版本見原始輸出','#fff2d9')
  s+=box(10,410,240,80,'測試文件與結果','本地證據目錄','#fff2d9')+box(350,410,200,80,'獨立取消對照','容器可停；尚未整合')
  s+=line(230,32,315,32,'模型請求',240,25)+line(315,70,230,70,'回覆',253,86)+line(505,32,590,32,'轉送',531,25)+line(590,70,505,70,'回覆',531,86)
  s+=line(250,251,350,251,'模型／工具描述',251,240)+line(350,293,250,293,'工具要求／答案',251,310)
  s+=line(590,270,550,270,'載入',555,258,True)+line(100,315,100,410,'讀檔／保存',112,365,True)
 s+='<text x="12" y="530" font-size="13">實線：請求與回覆　　虛線：資料存取或尚未採用的連接</text></svg>'
 return s
current=diagram(False);target=diagram(True)
(out/'current.svg').write_text(current);(out/'target.svg').write_text(target)
pages=[]
pages.append('''# AI 平台：全案決策版

2026-10-24｜沿用原工作區與原 LOOP｜重大方向均未定案

**這一版可供決策，整體產品仍未完成。** 目標是讓自己的介面收任務、讀必要資料、使用模型與工具，並能看結果及停止。Gateway 繼續提供模型入口。

## 現況：已有零件，尚未接成自家產品

'''+current+'''

本圖區分現有入口與獨立測試，沒有把小測試畫成已整合。正式模型路徑依原始碼整理，本輪未重送正式 Client；本地模型往返有原始輸出。

證據：原工作區 `src/adapters/provider.rs`、`src/services/openai.rs`；`docs/evidence/agent-platform/followup/model-tool-results.json`。現況快照是當時觀測，不保證服務現在仍為相同版本。
''')
pages.append('''# 目標：各區塊怎麼連接

**示意目標，全部不表示已部署。** 先決定任務由哪一層管理，再選底座。把圖上的任務程式放在 Gateway 裡，與獨立服務的改動範圍不同，不能默認任何一種。

'''+target+'''

任務程式整理上下文，也就是本次給模型參考的資料；它接收模型提出的工具要求，核對權限，再交工具執行區。模型憑證留在服務端。結果回介面，必要的來源與對話保存在獲准的位置。

快取放在模型服務一側；先重用相同開頭的計算，不先重用可能過期的工具結果。資料搜尋與雲端算力都是按缺口增加，不是首版前置。
''')
pages.append('''# 七個區塊：現況、選項與採用條件（1）

| 區塊 | 已有證據與缺口 | 候選、代價與驗收 |
| --- | --- | --- |
| Gateway | 原始碼有 Ollama 接法及快取用量欄位；這不是正式請求成功證據。自家介面尚未接入。 | 沿用 Rust Gateway，改動較少；替換核心需重驗相容性，本輪不建議。選定底座後，原 Client 與新介面各實送，確認模型、權限、錯誤及用量均正確。 |
| 上下文與資料品質 | 短摘要保留三項限制，刪掉限制的錯誤摘要會被拒；長對話、過期文件、來源衝突未測。 | 先用檔案搜尋與帶來源摘要；需要按意思搜尋時，才比較 PostgreSQL 全文搜尋與 pgvector。後者要維護文字轉數字的模型及索引。用相同問題測來源、版本、限制是否完整，不能只看回答流暢。 |
| 快取 | 本地工具往返第一筆回報 0、第二筆回報 334 個已快取輸入單位。不是正式帳單或控制變因實驗。 | 先保留供應商／本地服務的原生快取；vLLM 可重用相同開頭計算。自建答案快取會增加過期與資料隔離問題，暫不做。後續固定模型、題目與長度，分首次／重複請求，核對用量及耗時。 |
| 本地模型 | 既有 Ollama 上 gpt-oss 完成短題，Qwen 衍生模型完成一次讀檔往返；不代表官方 Qwen 品質。 | 先沿用 Ollama；llama.cpp 留為需調整硬體使用方式時的候選。換引擎有模型格式、工具回傳與記憶體代價。用完整任務比較正確率、時間及記憶體峰值，未測前不排名。 |

來源：本地 `local-measurements.json`、`followup/summary-checks.json`、`followup/model-tool-results.json`；[Ollama 工具呼叫](https://docs.ollama.com/capabilities/tool-calling)、[pgvector](https://github.com/pgvector/pgvector)、[PostgreSQL 全文搜尋](https://www.postgresql.org/docs/current/textsearch.html)、[vLLM 快取](https://docs.vllm.ai/en/stable/features/automatic_prefix_caching/)。

**新確認的範圍：** 334 是原始回覆提供的計數；沒有計算成金額，也不能用兩筆不同內容的耗時差，宣稱快取造成多少加速。
''')
pages.append('''# 七個區塊：現況、選項與採用條件（2）

| 區塊 | 已有證據與缺口 | 候選、代價與驗收 |
| --- | --- | --- |
| 工具與電腦操作 | 檔案搜尋、模型讀檔往返有輸出。Codex 獨立命令取消後仍寫檔；容器對照可停，但未整合。網頁與桌面全流程未測。 | 檔案工具優先；網頁用 Playwright 依按鈕和欄位定位；只有畫面的程式再評估 UI-TARS。後者需處理畫面辨識錯誤。完整流程須驗無權限寫入被拒、取消後沒有後續動作；不要只看取消回覆。 |
| 自家操作介面 | 本機 Codex 0.163.0 產生的接入規格包含對話、核准與中斷；尚無本案可用介面。 | A：Codex app-server，沿用現有執行程式；B：Qwen Code，以本地路線優先；C：OpenHands，以遠端工作區優先。都要做自己的介面接法，不能把 CLI 當完成。驗送任務、結果、核准、拒絕、停止、續接。 |
| 雲端 GPU | 本案沒有租卡、速度或價格實測，現有本機測試也未證明必須租卡。 | 先維持不租；不足時比較官方 API 與自租 GPU 上 vLLM。租卡另有閒置、儲存和維運成本；API 要評估資料外送。先訂實際需求及預算，再由使用者授權，不能只比每小時價格。 |

來源：[Playwright 定位](https://playwright.dev/docs/locators)、[UI-TARS](https://github.com/bytedance/UI-TARS)、[Qwen Code](https://github.com/QwenLM/qwen-code)、[OpenHands SDK](https://github.com/OpenHands/software-agent-sdk)。Codex 接入能力依本機產生的 `codex-schema/v2/`；取消證據在 `cancellation/`。

## 建議與事實分開

建議先保留 Gateway，任務管理獨立放置，先做單人、檔案任務；這是縮小改動面的工程判斷，**尚未被選定**。它不等於選 A，也不等於批准 Codex 底座。

不增加多套模型引擎、永久記憶或雲端服務，能減少第一條流程的變數。若使用者優先要求本機資料或多人遠端，第一個底座與驗收順序就應改變。
''')
pages.append('''# 重大方向：需要分開決定兩件事

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
''')
pages.append('''# 實作順序與整體驗收

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
''')
pages.append('''# 證據索引與停止位置

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
''')
md='\n\n<div style="break-after:page"></div>\n\n'.join(pages)
base.with_suffix('.md').write_text(md)
html=md_to_pdf.render(md,'AI 平台：全案決策版')
html=html.replace('</style>','h1{font-size:19pt;margin-top:0}body{font-size:10pt;line-height:1.5}table{font-size:9pt}svg{max-height:130mm}code{overflow-wrap:anywhere;font-size:8pt}</style>')
base.with_suffix('.html').write_text(html)
with sync_playwright() as pw:
 browser=pw.chromium.launch(executable_path=str(md_to_pdf.DEFAULT_CHROME),headless=True)
 page=browser.new_page()
 page.set_content(html,wait_until='load')
 page.pdf(path=str(base.with_suffix('.pdf')),format='A4',print_background=True,display_header_footer=True,header_template='<div></div>',footer_template='<div style="font-size:8px;width:100%;text-align:center;color:#777">研究決策版・未部署 | <span class="pageNumber"></span> / <span class="totalPages"></span></div>',margin={'top':'16mm','bottom':'18mm','left':'16mm','right':'16mm'})
 browser.close()
(out/'document-manifest.json').write_text(json.dumps({str(base.with_suffix(e)) : hashlib.sha256(base.with_suffix(e).read_bytes()).hexdigest() for e in ['.md','.html','.pdf']},indent=2))
print(base.with_suffix('.pdf'))
