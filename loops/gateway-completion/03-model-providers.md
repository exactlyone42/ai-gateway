# 03 模型、協定與選路

目標：圖中 Anthropic、OpenAI、Gemini、xAI、Ollama／vLLM 各依使用者自己的設定可用。

- 核對既有供應商實作、模型登記、政策及直接轉送設定；不把程式支援當已連線。
- 各來源記清楚原生或轉換格式、帳號來源、設定、能力差異、錯誤與串流；避免重送有副作用請求。
- 配合 05 的上下文管理，逐來源核對模型上下文上限、計數方式與供應商壓縮功能是否支援；轉換請求時保留工具呼叫與結果的對應關係，不能因模型部署位置自動改寫 Client 對話。
- 驗收：每個來源以合法可用的個人帳號或本機模型完成真實 Client 請求及串流，核對用量、錯誤及來源；未提供帳號不得宣稱通過。明確不可支援的格式需清楚拒絕並有陰性對照。

狀態 RUNNING；02 的第二位真人、跨主機與完整安全驗收仍未完成，留待整合階段，不視為通過。

2026-10-29：本機 Ollama 有 `gpt-oss:20b`；以獨立程序唯讀既有模型檔、限制兩個 CPU 核心，隔離 Gateway 使用自己的測試資料庫及個人 Gateway 金鑰連線。第一個串流請求在 180 秒沒有取得上游回應，Gateway 回 502；模型載入約 71 秒，之後 CPU 推論很慢。已用已載入模型與較短輸出做第二個獨立測試，仍在執行（測試程序 session 68492，隔離 Ollama session 78798）；不可稱本機模型串流通過。測試完成後需停止這兩個自有程序與隔離資料庫。啟動隔離 Ollama 時自動建立了 `/home/cash/.ollama` 的本機識別金鑰，保留未刪除；既有服務未修改。

第二次使用已載入模型與最多 4 個輸出 token：隔離 Gateway 收到串流結束標記與供應商用量，測試通過；Ollama 記錄推論約 122 秒。測試只確認串流資料及用量，沒有確認有可讀答案，也不是原 Client 或可接受速度的驗收。隔離 Ollama、Gateway 與資料庫已停止；既有服務未修改。

查明 OpenAI 格式的原生串流在上游標頭出現前不會對 Client 發等待訊號；新增測試，修正前在一秒內未回應而失敗，修正後會送等待訊號並正常轉送結束事件。上游延後回錯誤時，測試確認只送一次、回通用串流錯誤且不洩漏供應商金鑰；靜態檢查通過。等待訊號不延長上游閒置逾時，也不覆蓋轉換格式；完整記帳、斷線、原 Client 與本機慢模型仍待驗證，尚未部署。

隔離資料庫測試確認等待訊號後上游回 429 時，請求與供應商嘗試都記為失敗；故意改為完成時測試失敗。使用原生 TCP 模擬上游，確認 Client 在等待期間關閉後，上游連線也會關閉。Responses 與 Chat 的延後錯誤改用各自格式；尚未以原 Client 確認錯誤事件，也未涵蓋協定轉換或真實慢模型，未部署。

OpenAI 格式轉往 Anthropic 的延遲上游測試，修正前等待訊號測試逾時；改為轉換路徑也先送等待訊號後，能取得轉換後的正常結束事件。隔離資料庫下 58 項串流相關測試及靜態檢查通過；靜態檢查要求調整記憶體配置後，轉換測試再次通過。尚未以原 Client 驗證延後錯誤或真實慢模型，未部署。

以目前 Gateway 程式、隔離資料庫、延遲回應的模擬供應商及原 Codex CLI 0.164.0 實測：等待訊號後的正常串流可完成；供應商延後回 400 時 CLI 回報失敗，供應商只收到一次請求。測試使用暫時模式與測試金鑰，未使用真實 OpenAI 帳號、真實慢模型或正式服務；測試程序及資料庫已停止。

再將模擬供應商開始回應的時間延長到 65 秒：原 Codex CLI 約 66.6 秒完成，供應商只收到一次請求；延後錯誤仍被 CLI 判定失敗且沒有重送。另以程式測試確認 Chat 格式延後失敗時回傳對應錯誤、不洩漏供應商金鑰且只呼叫一次。這不等於真實慢模型或經正式代理的長串流驗收；隔離程序與資料庫已停止。

因實測本機模型載入較久，新增 Ollama／vLLM 模型請求專用的上游等待設定，預設五分鐘；其他來源保持原設定，Compose 已傳遞設定。隔離資料庫測試確認本機模型路徑可以等到較慢回應、雲端仍按原逾時；暫時改回共用逾時時，本機路徑測試失敗。59 項串流相關測試、靜態檢查與 Compose 設定檢查通過；真實模型與原 Client 在新設定下尚未重測，未部署。

以新版 Gateway、隔離資料庫及獨立 Ollama 程序重測真實 `gpt-oss:20b`，限制六個 CPU 核心；雲端逾時設 25 秒，本機沿用五分鐘。先以 16 個輸出 token 測試，串流完成但沒有可讀文字；提高上限後，第二個請求約 164.4 秒取得非空文字、正常停止與供應商用量。此測試使用通用 HTTP Client，非原 Codex；模型速度仍慢，未部署。隔離模型、Gateway 與資料庫已停止，既有 Ollama 未修改。

目前環境及正式 Gateway 未發現 OpenAI、Gemini、xAI API 金鑰，亦未發現 vLLM 服務，不能宣稱這些來源已真實連線。隔離資料庫及模擬 Gemini、xAI 測試確認兩位使用者各自的個人金鑰、一般回應與串流回應；未持有該來源金鑰的人在一般及串流請求都被拒絕，供應商沒有收到額外請求，回應不含供應商金鑰。測試與靜態檢查通過；真實帳號及原 Client 實送仍未驗收。

以原 Codex CLI 0.164.0、隔離 Gateway 與真實本機 `gpt-oss:20b` 執行簡短任務：六核心限制下約 227 秒完成，CLI 回傳一則非空答案；隔離帳本有 1 筆完成請求與 1 次上游嘗試。測試只確認有可讀回覆，未核對答案是否完全符合題目，也未驗工具操作或可接受速度。隔離模型、Gateway 與資料庫已停止，既有服務未修改。

2026-11-05 本人Codex現成來源唯讀盤點：/home/cash/.codex/auth.json為chatgpt模式，access token與account id均存在（值未輸出）；使用該登入對官方https://chatgpt.com/backend-api/codex/models?client_version=0.166.0唯讀GET，HTTP200，模型清單含gpt-6-luna，證據/tmp/gateway-owned-codex-models.json。正式容器掛/run/gateway-codex但未讀出憑證。這不證明模型實送、原Client或CodexFile持有人路徑完成；下一步重用verify-client-identity既有隔離建立流程，以CodexFile本人來源、合成測試資料及單次短答驗證，並核對其他使用者與撤銷Gateway金鑰拒絕，不修改原登入檔。容量估算仍由其他session處理。

2026-11-05 本人Codex真來源後續：原Codex0.166.0經Gateway099a3b8-token-unlock向gpt-6-luna完成一次短答；同租戶非持有人503、撤銷Gateway金鑰401，兩者Client退出1且無上游。隔離帳本僅1次上游、1筆完成及provider_reported輸入13176／輸出9；來源登入檔未變。證據`/home/cash/.cache/tmp/gateway-client-identity-1l7aaokf/result.json`，驗收程式e8bd07d已推送。僅證明本人訂閱短答與隔離，不涵蓋真來源工具、真來源壓縮及保存；前段唯讀模型清單不再是此來源唯一證據。正式未動。

2026-11-05 最新Codex0.166.0／Ollama0.34.0真模型驗收未過：gpt-oss:20b工具首次 `/tmp/gateway-client-identity-n_e9ajbc/held-owner/client-diagnostic.txt` 發出container.exec、read_file及exec等未提供工具，後續502重連，為停止重複失敗而對該原Client送SIGINT；不是取消驗收通過。關閉request/stream重連並明確指定exec_command後，`/tmp/gateway-client-identity-w5058jwy/held-owner/client-diagnostic.txt` 確實呼叫exec_command但把隨機檔路徑寫錯，工具退出1後下一輪502。基本短答對照 `/tmp/gateway-client-identity-gn1g_xcj/held-owner/client-diagnostic.txt` 退出0卻回JSON而非要求的精確字串，嚴格驗收拒絕；不能以非空文字稱通過。三個失敗服務與DB皆已回收。Gateway502原因尚未證實，不把模型誤用工具與協定錯誤混為同因；直接Ollama Responses對照 `/tmp/gateway-ollama-responses-probe.headers` 及 `.sse`為200 SSE與response.completed。失敗診斷初版在保存匯入前查詢而得到空集合；原始碼startup.rs明確每30秒執行保存維護，改為有界等待最多50秒後才讀回。下一步用既有qwen3-coder:30b測程式工具模型並讀失敗加密內容。官方來源 https://docs.ollama.com/api/openai-compatibility ，原件 `/tmp/gateway-ollama-openai-reference.html`；僅非stateful Responses受支援。正式環境未動，未修改Ollama設定或新增模型。

2026-11-05 本機互通查明並修兩個產品缺陷：其一，原Codex實際送reasoning.effort=none、summary=auto，Gateway把明確關閉推理誤當推理能力需求，拒絕supports_reasoning=false模型；第一次僅排除effort後仍被summary觸發，已按實際組合修正。陰性 `/tmp/gateway-local-reasoning-negative.log` 與 `gateway-local-reasoning-summary-negative.log`均失敗，新版model_registry12項過。其二，同一真工具請求直接重播至Ollama0.34.0，200回application/x-ndjson但body是完整SSE，見 `/tmp/gateway-local-tool-replay.headers`、`.sse`及 `/tmp/gateway-client-identity-1mhforzg/failure-captures.json`；Gateway原誤走buffered JSON解析而502。只在Ollama Responses的stream模式將該成功標頭正規為text/event-stream，內容不改；Ollama非串流／Chat、vLLM及OpenAI不套用。舊版header陰性 `gateway-ollama-header-negative.log`確實失敗；新版provider15項及原生observer6項過，含真正NDJSON拒絕。官方0.34.0 Responses原件 `/tmp/gateway-ollama-responses-reference.go`與官方推理設定原件 `/tmp/gateway-ollama-reasoning-reference.go`僅作行為參考，實測標頭才是本機問題證據。

2026-11-05 新核心3f4d824-ollama-stream：原Codex0.166.0經真本機qwen3-coder:30b完成一個exec_command讀隔離隨機檔、工具結果回傳與精確答覆。`/tmp/gateway-client-identity-nwib6xcz/` 的result、capture-result、usage-result、plaintext-result.json：2上游、2筆complete、8方向加密讀回及provider/client串流相等；完整原user與工具call id／結果保留；逐筆用量核供應商完成事件，總輸入28297、輸出70。模型權限拒絕403及金鑰撤銷401均Client退出1、沒有額外上游；指定隨機值raw/hex掃2份spool、DB dump及一般log過。驗收先讀/api/show，按模型能力選none；不支援工具或文字的模型直接拒絕測試。兩次Client重連均設0。此為隔離的真本機模型驗收，不是容量或任意模型品質保證；gpt-oss未過仍保留。服務與隔離DB已回收，現有Ollama設定未改、正式Gateway未動；新版全套、提交與CI待辦。

2026-11-05 本機互通修正版全套 `/tmp/gateway-ollama-core-all.log` 結束碼0：Rust主庫476過、7 ignored，其餘整合測試通過；Client驗證38過，備份相關159項含20 skipped，守門陰性33/33過。差異自查確認產品改動限於關閉推理能力判斷及Ollama Responses串流標頭，未放寬內容解析與完成事件檢查；本輪未獨立審查，P3維持RUNNING。Claude本機路線下一步先驗既有Messages轉Responses路徑；官方Anthropic相容及Claude Code文件已讀，原件 `/tmp/gateway-ollama-anthropic-reference.md`、`/tmp/gateway-ollama-claude-reference.md`；文件不能代替0.34.0原Client實送。提交、新CI與正式驗收尚未完成。

2026-11-05 Claude本機路線盤點：`src/adapters.rs::validate_cross_request` 明確拒絕Anthropic Messages串流轉Responses，因Responses終點才提供輸入用量，不能宣稱原Claude能走該轉換。現有LocalProvider只宣告OpenAI方言，EXTRA_PROVIDER_PROFILES_JSON也只允許Ollama的Responses／Chat，故原生Messages尚未接通；官方本機Anthropic相容接口提供Messages、工具與串流，不支援count_tokens。下一切片以獨立Ollama Messages設定沿用原生接口，不把串流緩衝至完成來偽裝互通；保留既有OpenAI路線及跨協定拒絕規則，先做設定與供應商正反對照，再原Claude實送。官方文件是研究來源，0.34.0實送仍待驗。
