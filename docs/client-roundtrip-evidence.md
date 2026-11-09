# 原 Client 傳輸驗收

## 2026-11-10 R2-S4 撤權、更新與最後一筆額度（HTTP）

兩協定各30案成功，使用固定執行檔 `r1s3read-8f04a37-idempotent`；只新增驗收器，不改正式Rust、Client或使用者預設。以 `--check-access-race --client claude|codex` 執行，來源回應頭與本文可分別暫停；這是隔離HTTP及合成來源，未啟動原Client或真模型。

Gateway金鑰撤權成功後，兩次新請求皆401且零來源；先前已獲准、來源尚未回應的請求可完成，帳本仍歸原已撤金鑰。先暖快取，再讓舊請求跨過撤權才結束，確認不會使快取重新接受撤銷金鑰。現有身分單元測試另驗解析鎖阻擋撤權、釋放後401；不把HTTP一種先後順序冒稱全部競爭排列。

個人來源更新／撤銷、群組來源更新及移除會員，在來源回應頭尚未送回時皆503，原檔與會員保留。送回回應頭後，實際檔案鎖探測確認已釋放，此時舊本文仍暫停；明確再更新成功，新請求即用新來源。移除會員後的新群組請求503且零來源，B仍正常，恢復個人來源後A也正常。拒絕忙碌操作與撤權成功分開判定；不以等待時間猜測鎖界線。

透過管理API輪替Gateway金鑰，900秒重疊期內，新舊兩把同使用者金鑰並行搶每分鐘最後一筆額度，只得200／429及一次來源呼叫；同分鐘再送仍429。資料庫對回同一使用者、兩個不同credential、一筆已結帳保留、一筆最終事件，額度視窗仍為1。停用測試額度後新請求恢復。加 `--control-quota-double-admit` 將測試額度改2，實際兩200／兩來源，原驗收命令退出1，確實拒絕雙重核發結果。此反例檢查驗收判別力，不代表產品曾有超發缺陷。

每協定12筆完成及12筆來源回報用量（各輸入1、輸出1），2筆額度拒絕均無來源嘗試；全部60個案例入口編號與日誌狀態相符。早期401及缺來源503仍無帳本，未冒充記帳。已變更的測試來源、現存兩份來源檔內容及完整Gateway金鑰格式均未出現在一般日誌或查帳輸出。重疊到期、第二真人、跨機及同版發布未驗；完整原件與檢查結果見 `/home/cash/.local/state/ai-gateway/r2-access-race-20261110/summary.json`。S1／S3同執行檔證據沿用，不重跑未變矩陣。


## 2026-11-10 R2-S3 跨租戶與角色隔離（HTTP）

同一隔離資料庫、同一Gateway，以真管理API建立兩租戶及Admin/Operator/Viewer。兩邊模型與使用者同名、憑證不同；每協定36案，共72案通過。各有6次來源成功、1次無效供應商憑證401且不回退、23次權限拒絕零來源、6次管理允許。此為HTTP合成身分及來源，未重跑原Client或真模型。

| 角色 | 查本租戶使用者 | 建群組 | 讀／設定憑證、加會員 |
| --- | --- | --- | --- |
| Admin | 允許 | 允許 | 允許 |
| Operator | 允許 | 允許 | 拒絕 |
| Viewer | 允許 | 拒絕 | 拒絕 |

跨租戶物件讀寫刪404；A政策允許B專用模型，仍由租戶模型清單拒絕403。格式有效但秘密變更的Gateway金鑰401不上游，與供應商401的一次來源呼叫分開。B兩份憑證檔保持原輸入；全部入口、帳本歸屬及秘密掃描對回。依[OWASP物件授權指引](https://api-security.owasp.org/editions/2023/en/0xa1-broken-object-level-authorization/)核對物件權限，角色表僅涵蓋本次操作，非所有管理API窮舉。

故意用Admin代Viewer回200，驗收器拒絕冒403。首件因未明列來源可信租戶而503的失敗保留，後續僅明確允許兩測試租戶。新入口 `--check-tenant-access`，角色反例加 `--control-tenant-admin-as-viewer`。只補驗收器；同binary的S1群組歧義/硬限制證據沿用，競態、保存及同版發布仍待。原件、全套與提交狀態：`/home/cash/.local/state/ai-gateway/r2-tenant-access-20261110/summary.json`。

## 2026-11-10 R2-S2 本人Codex首次使用

原Codex 0.166.0沿目前設定的 `gpt-6.1-sol`，完成管理API建立身分、來源、政策及Gateway金鑰，到本人訂閱首次使用與帳本對回。明確綁定唯一持有人；來源只複製access/account至0600隔離檔，不含refresh，原登入及預設路線未改。[官方說明](https://developers.openai.com/siwc/token-sharing-open-source/codex-app-server)亦以實際完成推論確認當次模型可用，登入檔存在不足以證明。

本人退出0、Gateway一筆完成嘗試，供應商回報輸入13184、快取輸入3712、輸出9；完整加密保存四段讀回相符。非持有人503、撤權後401，原Client均退出1，未增加供應商嘗試。三組隔離設定皆零請求及串流重試；只改驗收器一行，不改正式Rust。來源秘密未出現在Client診斷、一般日誌或資料庫；原檔及隔離快照雜湊未變。

這是本機唯一Codex持有人、真訂閱來源與管理API路線，未驗UI、第二真人、多帳號或正式部署。真HTTPS來源沒有服務端計數器，原結果的 `upstream_requests` 是帳本推定；只能說一筆Gateway嘗試，不能冒實體HTTP總數。此次帳本無供應商請求編號，完整追查仍待發布前核對。其他來源分支、R2其餘安全及R3未完。原件、全套與提交狀態見 `/home/cash/.local/state/ai-gateway/r2-first-use-20261110/summary.json`。

## 2026-11-10 R2-S1 本人憑證選擇（隔離HTTP）

沿既有建立身分、金鑰、來源及政策API，兩協定各23案：13案成功且僅呼叫正確來源一次，10案拒絕且不上游。涵蓋個人優先、缺綁定、個人損壞不回退、群組歧義與摘要不符、群組硬限制及零額度、撤權後拒絕；B仍可使用。錯用B假憑證即使HTTP200仍被驗收器拒絕。

本輪只補驗收器，以 `--check-access-matrix` 執行；`--client` 選HTTP協定，沒有啟動原Client生成。來源及身分均為合成測試資料，不冒真人、合法正式憑證、跨主機或並行撤權競爭。個人未設定才查唯一群組，錯誤拒絕；[OWASP權限指引](https://cheatsheetseries.owasp.org/cheatsheets/Authorization_Cheat_Sheet.html)支持逐次核權與預設拒絕。

每協定13筆完成、3筆政策／額度拒絕歸屬相符；5筆憑證選擇503發生在帳本建立前，另2筆認證401亦無帳本，全部可由入口編號對回一般日誌。不能宣稱所有失敗都已記帳。初件租戶反例沒改到金鑰、錯估帳本21筆的失敗原件保留；實際為16筆。完整原件、檢查及發布狀態見 `/home/cash/.local/state/ai-gateway/r2-access-matrix-20261110/summary.json`；未改正式Rust、Client預設或正式環境。

## 2026-11-10 R1-S4 暫時故障後的新請求

原Claude 2.1.319及Codex 0.166.0，沿用固定執行檔 `r1s3read-8f04a37-idempotent` 與隔離零自動重送設定，各完成429限流、503暫時不可用，以及部分文字後完全停止送資料的閒置逾時。六組各一次失敗退出1，再另發一次新請求成功退出0；來源各一次，兩個入口及嘗試編號、保存與用量可逐筆對回。來源為受控假供應商，不是正式模型故障或預設Client重試策略的驗收。

429/503的錯誤JSON有完整保存，帳本仍為失敗；完整保存不等於請求成功。逾時的首筆保存不完整，恢復筆完整且成功。已收到的部分用量照實保留，未收到者仍只有估算，不把0當免費或最終費用。429/503來源附等待1秒提示；測試在錯誤後等待1秒才另發新請求，不宣稱Client自行等候或重送。

新選項為 `--check-error --check-capture --check-transient-recovery rate-limit|unavailable|timeout`，兩Client與每個模式分開用新隔離庫及臨時服務。逾時模式只在隔離Gateway設定 `PROVIDER_IDLE_TIMEOUT_MS=250`；[程式所用reqwest 0.12.28說明](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html#method.read_timeout)明定這是每次讀取的等待上限。來源保持連線最多2秒，兩個案例實際約257毫秒收到Gateway提前關閉；另用 `--control-timeout-not-enforced` 將等待改5000毫秒，來源2秒自行結束，即使Client仍退出1，驗收也確實拒絕冒稱閒置逾時。

Codex429初件只因驗收器要求來源測試字串而失敗；原Client實際已顯示429及Gateway請求編號。原JSON失敗事件重播與入口編號一致，初件整個命令仍保留失敗；修正只接受單一429失敗事件與實際完整編號，不放寬其他錯誤，新案例才完成恢復。SDK的「exceeded retry limit」文字不證明真的重送，這裡實測只有一次來源呼叫；[官方錯誤格式](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/error.rs)只輔助說明，本機固定版實際輸出才是證據。

本輪只補驗收器，局部88項通過，含7項新失敗對照；完整檢查、提交推送與CI見 `/home/cash/.local/state/ai-gateway/r1-s4-transient-recovery-20261109/summary.json`。獨立qwen3-coder審查未指出缺陷，但未逐段說明必要性，不冒完整簡化通過。Rust、Client執行檔、使用者預設與正式環境未改，未合main或部署。

限制保留：候選一般日誌及帳本仍把斷線/閒置統稱傳輸錯誤，本輪來源觀察才可區分；只有Codex429/503此次顯示Gateway編號，其他四案沒有，但API及帳本可對回。連線與回應頭等待逾時、所有5xx種類、原Claude503歷史因果、S2格式、整體同版、安全/恢復/發布及舊範圍仍未全驗，不將六組通過冒稱產品完成。

## 2026-11-09 R1-S3 慢速接收與取消後恢復

新增容量模式 `--check-slow-reader --check-cancel --check-capture`，以原有固定執行檔 `r1s3read-8f04a37-idempotent`、新隔離庫及受控假來源測試。先由原始HTTP接收端暫停、每250毫秒讀16KiB，再有界追上一段並再次暫停；原Claude／Codex只執行取消後的新請求，不冒稱原Client本身被限速或真模型已驗。

兩協定各一組通過：來源停止、恢復讀取後再前進，未耗盡64MiB來源上限；量測唯一Gateway執行檔PID，沒有量到timeout父程序。記憶體增量各約3.24／3.25MiB，未讀積壓峰值約5.31／5.13MiB，均低於本案例32／8MiB門檻。這是64KiB完整SSE區塊及本機單筆工作的範圍，不能當所有負載的效能保證。

接收緩衝在連線前設定，讀回實值8192位元組；[Linux TCP說明](https://man7.org/linux/man-pages/man7/tcp.7.html)指出核心會加倍保留空間。設定值本身不算背壓證據，仍要求兩段各至少8個來源停滯樣本及中間來源恢復。未讀量扣除已被讀走的資料，不能把總傳送量全當積壓；首次只讀192KiB、錯用總量與短穩定窗的失敗保留，沒有降低原穩定樣本或記憶體門檻。

關閉接收端後，兩來源確實斷線、Gateway工作歸零；同來源及模型各限一筆的設定下，原Client新請求各一次成功。各兩筆保存為不完整／完整、帳本為取消／成功，八段授權讀回與用量對回。取消時Client保存只要求為來源原文前綴，不能虛稱兩側完整相等；其餘模式仍逐字比對。一般日誌、DB及暫存明文掃描通過。

快讀反例確實被拒絕；五項本機回歸驗證來源未停、過量記憶體、未消費積壓、來源耗盡、短窗口與篡改前綴。完整檢查、提交與CI以 `/home/cash/.local/state/ai-gateway/r1-s3-slow-reader-20261109/summary.json` 為準。未改正式Rust、Client預設或正式環境，未合main／部署；原Claude503因果、S2格式、其餘故障與全部安全/發布/舊範圍仍保留。

## 2026-11-09 R1-S3 同份保存重播與授權讀取

背景匯入重播相同內容時，仍重寫相同狀態，會使使用既定資料快照的授權讀取被資料庫拒絕，錯誤碼為40001。原加密資料的隔離複本先有96次普通讀取成功；受控安排同時加鎖而不更新時200，相同狀態重寫時503，不能只憑多次成功排除競爭。

現在最後狀態更新只在狀態不同時執行；原批次內容核對、衝突整筆回滾、鎖與權限檢查保留，沒有重試請求或降低交易隔離。讀取核對失敗另記固定階段及SQLSTATE，不記原SQL、參數、內容或憑證。實際匯入回歸原版1項確實以40001失敗，修正後保存模組15項通過。修改後同一執行檔反向套回舊SQL仍503且明記40001，新SQL則200、內容雜湊相同。

固定執行檔 `r1s3read-8f04a37-idempotent` 已將原兩筆保存八段全部授權讀回，內容對回；原DB與複本均為2筆、836塊，內容雜湊相同，用量嘗試仍2筆。沒有新模型或原Client生成請求，沒有改原DB、使用者預設或正式環境。這是保存讀取路徑證據，不能代替新版原Client完整工作流程。

原Claude首件503未記SQLSTATE，仍無法回溯證明就是此原因；原失敗不改判。獨立小差異審查誤解資料庫隔離與鎖且截斷，未獨立通過。完整檢查、實際提交推送與CI見 `/home/cash/.local/state/ai-gateway/r1-s3-capture-read-20261109/summary.json`。S3仍驗證中，慢讀、其餘故障、安全、同版發布及舊範圍保留；未合main或部署。

## 2026-11-09 R1-S3 真模型取消與新請求恢復

同一既有執行檔 `r1s4auth-d251c59-unlock`，原 Claude 2.1.319／qwen3.8:27b 與 Codex 0.166.0／qwen3-coder:30b，實際生成後 SIGINT 取消。依模型檔精確對回本機唯一推理工作，從處理中轉閒置分別約 0.44／0.14 秒；這是單次樣本，非效能保證。Gateway 工作歸零，兩者再另發同模型新請求都成功；未直接證明底層連線關閉。

Codex 完整命令通過。Claude 首次讀取取消保存的 provider_request 回 503，原命令仍失敗；失敗處理留下的原八段內容經重播驗證通過，沒有重送模型或改判成功。原因未明；對應三秒沒有 Postgres 錯誤日誌，不能推定資料損毀或交易衝突。兩者取消保存不完整、帳本取消，恢復保存完整、帳本成功。Claude 已回報輸入11059保留，未完成輸出未知；Codex 取消尚無供應商用量，只保留估算，零值不代表免費。

沿用新隔離庫與臨時服務登記，執行 `--check-local --source-model <上述模型> --check-cancel --check-capture`；沿用原零重送及 Claude 停用非串流備援設定。生成等待最多240秒、取消退出5秒、推理停止10秒；不停止或卸載模型服務。真 Codex 生成後強殺 -9 的反例確實拒絕，原件保留。

81項保存驗證通過；全套 Rust538／Python260／前端22通過，Rust6／Python20未跑。錯誤縮排在服務前被語法檢查拒絕；獨立程式審查100秒逾時無報告。未改正式 Rust、Client執行檔或使用者預設，未合main／部署。S3仍驗證中：先查保存503，再補慢讀有界；其餘原範圍保留。完整原件：`/home/cash/.local/state/ai-gateway/r1-s3-real-cancel-20261109/summary.json`。

## 2026-11-09 R1-S3 受控長串流與取消

本輪沿用 `r1s4auth-d251c59-unlock` 固定執行檔，只補驗收器，未改正式請求程式、Client 執行檔或使用者預設。來源是本機可控的 Anthropic／OpenAI 串流，不冒稱指定實際模型、正式反向代理或跨機已驗收。

原 Claude 2.1.319 與 Codex 0.166.0 各以一次來源請求開始串流，再收到 SIGINT 中斷訊號。兩側連線關閉，保存標為不完整、帳本為取消；Claude 已回報的輸入 1／輸出 0 保留，Codex 未收到供應商用量仍只有估算，不把實際欄位的零當免費。另補取消後 Gateway 執行中請求必須歸零，兩 Client 已用新隔離庫重驗均為 0；這不證明實際模型的推理工作已停止。強制以 SIGKILL 結束 Claude 的反例退出 -9，確實被拒絕，不能冒充正常取消。沿用 `--check-cancel --check-capture`；反例另加 `--control-force-client-kill`。

長串流模式為 `--check-long-stream --check-capture`，固定真實 660 秒逐字傳送，不能由命令參數縮短。Claude 的閒置與首筆等待、Codex 的串流閒置均在隔離設定設為 600000 毫秒；兩種 Client 均停用自動重送。依 [Claude 官方等待設定](https://code.claude.com/docs/en/env-vars) 及 [Codex 官方設定](https://learn.chatgpt.com/docs/config-file/config-reference)，這些是等待不到新資料的限制；660 秒總長不等於能容忍 660 秒完全無資料。這裡每個字相隔約 41 或 60 秒，Gateway 也保留自己的保活訊息。

驗收必須同時有真實首尾時間超過 600 秒、唯一來源請求、原 Client 完整預期文字與退出 0、唯一完成事件、完整加密保存與成功用量；不能只憑等候時間或保活判成功。兩原 Client 實際各持續 660 秒、一次入口及上游、退出 0，保存完整且供應商用量各輸入 1／輸出 1，長串流兩項通過。保存驗證新增五項，共 74 項通過；短完整回覆、長時間截斷、錯字及重複完成的四項斷言在補強前確實失敗。完整檢查及推送狀態，以 `/home/cash/.local/state/ai-gateway/r1-s3-stream-cancel-20261109/summary.json` 最終原件為準。

每個模式與 Client 都使用新建的本機 `gateway_client_identity_validation_` 隔離庫、明確寫入旗標及臨時服務登記；原 Client 長串流上限 780 秒、Gateway 900 秒，失敗不盲重送、不清庫。S3 維持驗證中：指定實際模型取消、慢讀取有界記憶體與資源釋放仍需證據；S2 格式、S4 其餘故障、S5、R2／R3 與舊範圍保留，未合 main 或部署。獨立程式審查 100 秒逾時無報告，不稱已獨立通過。

## 2026-11-09 R1-S4 部分輸出後斷線

同一既有執行檔 `r1s4auth-d251c59-unlock`，原 Claude 2.1.319 與 Codex 0.166.0 各收到部分文字後，由假供應商提前關閉連線，不送完成或錯誤事件。兩個 Client 均退出 1、只有一次上游請求；再另發新請求，各一次成功、退出 0，共四項通過。本輪只補驗收器及使用設定，沒有改正式請求程式或 Client 執行檔。

Claude 原設定共送六次且逾時；只設 `CLAUDE_CODE_MAX_RETRIES=0` 仍有四次，包含改發非串流。須再設 `CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK=1` 才通過一次上限，使用範例已補齊。兩設定分別依 [官方重試說明](https://code.claude.com/docs/en/errors#tune-retry-behavior) 與 [環境設定](https://code.claude.com/docs/en/env-vars)；Gateway 模型清單中的零重試欄位不足以證明 Claude 不重送。Codex 沿用 S1 的零一般重試及零串流重試設定。上述設定只用於隔離 Client，未改使用者預設。

每個 Client 都有兩筆不同入口與保存：首筆不完整、失敗原因為上游內容傳輸錯誤；後筆完整、成功。Claude 已收到供應商回報的輸入 1／輸出 0，仍照實保留；Codex 尚無供應商用量，只有估算，不能將實際用量欄位的零當成確知免費。恢復請求各回報輸入 1／輸出 1。兩側原始內容、失敗編號與帳本逐筆相符，日誌、資料庫及加密檔的測試明文掃描通過；不要求原 Client 一定把部分文字顯示在畫面上。

既有驗收器加入 `--check-error --check-stream-failure --check-capture`，兩個 Client 分別使用新建且名稱以 `gateway_client_identity_validation_` 開頭的本機隔離庫，設定 `CLIENT_IDENTITY_ALLOW_DATABASE_MUTATION=1`，透過既有臨時服務工具登記。每個 Client 上限 30 秒，保存最多等 50 秒；沿用前輪認證拒絕的固定執行檔與版本。`--control-error-as-success` 使來源回完整成功串流，驗收確實拒絕退出 0 的假錯誤；八項新保存測試另拒絕未開始、已完成、狀態錯誤、遺失或虛構用量，保存驗證共 69 項通過。

原件與失敗對照：`/home/cash/.local/state/ai-gateway/r1-s4-stream-interruption-20261109`。來源為可控假供應商，不證明真實供應商、實際工具執行或長串流取消正常。其餘 429／5xx／逾時、S2 格式、S3／S5、R2／R3 與舊範圍仍未驗，S4 維持驗證中；未合 main 或部署。完整檢查與推送狀態以該處 summary.json 為準。

## 2026-11-09 R1-S4 上游認證拒絕後恢復

Gateway 原本丟掉上游的 `x-should-retry` 提示，原 Claude 經固定 401 來源共送六次並逾時。現在兩種協定都保留上游原值；兩項 HTTP 路由測試先失敗、修改後通過，既有狀態碼、原始內容與重試等待時間仍相符。

原 Claude 2.1.319 與 Codex 0.166.0 用同一執行檔 `r1s4auth-d251c59-unlock`，各完成一次 401 拒絕，再以另一筆新請求收到完整成功回覆，共四項通過。每個 Client 只有兩次上游呼叫、兩筆獨立入口及保存；錯誤編號可對回上游嘗試，拒絕不虛構實際用量，成功記帳與固定回應相符。來源為本機可控假供應商，只驗錯誤處理，不能據此宣稱真實供應商正常。

Codex 初件漏用 S1 已驗的零自動重送設定，雖正常退出仍送六次，保留為失敗；補齊驗收器與使用範例後才通過，沒有改 Client 執行檔或使用者預設設定。假供應商改回成功的陰性對照會被拒絕。保存驗證共 61 項通過，包括遺失或倒置錯誤、截斷恢復串流、重複編號、失敗冒成功、虛構用量與錯配上游編號。

從整合區執行既有驗收器，加入 `--check-error --check-auth-recovery --check-capture`；兩個 Client 各用新建的 `gateway_client_identity_validation_` 本機隔離庫及明確寫入旗標，透過 `temp-service.py` 登記。執行檔及版本如上；每次 Client 上限 30 秒，保存最多等 50 秒，失敗不清庫或盲重送。`--control-error-as-success` 是預期失敗對照。

完整檢查首件在初始化後立即讀取金鑰鎖時失敗；補上初始化完成後的明確解鎖，四個恢復階段的系統呼叫紀錄由零次解鎖變成各一次，首件保留。子程序繼承鎖是依檔案狀態及 [Linux 鎖規則](https://man7.org/linux/man-pages/man2/flock.2.html) 的推論，未拍到首件子程序。最終執行檔已重新驗兩個 Client。

原件與命令：`/home/cash/.local/state/ai-gateway/r1-s4-upstream-auth-20261109`。403、429、5xx、逾時及已開始工作後中斷不重送仍未驗；S4 保持驗證中。S2 格式、S3／S5、R2／R3 與舊範圍保留，未合 main 或部署。獨立審查實際逾時，不能稱審查全綠；完整檢查及推送狀態以該處 summary.json 為準。

## 2026-11-09 R1-S4 撤權後正常退出

Claude Code 2.1.319 對固定 401 做四組隔離對照：原錯誤內容及官方格式都在 30 秒內共送六次（首次加五次重試）、未退出；各自加上 `x-should-retry: false` 後，均約兩秒退出 1、只送一次。原件見 `/home/cash/.local/state/ai-gateway/r1-s4-20261109/cause-comparison.json`。因此只替 Gateway 自行判定的無效金鑰回應補這個標頭；錯誤碼、內容及暫時故障的 503 不變。

同一執行檔 `r1s4-d7a5b8a-header401` 經原 Claude／qwen3.8:27b 與原 Codex 0.166.0／qwen3-coder:30b 實送，各有成功短答、模型 403 拒絕及撤權 401 正常退出，合計六項通過。每個撤權案例的原 Client 僅送一次推論請求、零上游；另有一次直接 HTTP 預查，兩者分開計數。成功請求的保存與實際用量對回，原件在上述目錄的 `claude`、`codex` 子目錄。

驗收沿用 S1 命令，改用該目錄的固定 `gateway` 執行檔與新版本標記；各用新建的本機隔離庫。三項 Rust 錯誤測試通過；新增的 401 標頭斷言在修正前確實失敗。首輪全套停在測試排版；次輪在最後啟動掃描遇到舊測試庫的資料庫版本紀錄不符。改用全新隔離庫後，最終全套退出 0：Rust 538 次、Python 232 次、前端 22 項通過；Rust 尚有 6 項、Python 20 項未跑。舊庫與各次日誌保留。這只解決 S4 的 Gateway 撤權退出，上游認證錯誤、429、5xx、逾時、中斷及新請求恢復仍未驗；S2 格式失敗、S3、S5、R2／R3 也未完成。未部署或改使用者的 Client 設定。

## 2026-11-09 R1-S2 真工具與使用者續談

本輪沿用 S1 的 Gateway binary `f8f339c51e80`，只補驗收器，沒有改正式請求程式或部署。原 Claude Code 2.1.319 使用本機 `qwen3.8:27b`；原 Codex 0.166.0 使用已安裝的 `qwen3-coder:30b`。兩者各完成一次真讀檔、工具結果回送及使用者第二輪引用，共三個來源請求。兩側原訊息、工具編號、完整工具結果、串流及實際用量均有保存原件可核。

沿用 `verify-client-identity.py --check-local --check-capture --check-owned-tools`。每個 Client 須使用獨立且新建的本機 `gateway_client_identity_validation_` 測試庫，並明確設定 `CLIENT_IDENTITY_ALLOW_DATABASE_MUTATION=1`；透過既有 `temp-service.py` 登記 30 分鐘。Claude 的 `--source-model` 設 `qwen3.8:27b`，Codex 設 `qwen3-coder:30b`。第一輪及續談各限 600 秒，Gateway 限 1500 秒；失敗不自動重送，也不刪除資料庫或原件。

測試隨機檔的內容不出現在第一輪提示中。原 Client 以本機工具讀檔後，第二輪要求只引用已有結果，不准再次讀檔；Claude 使用 `--continue`，Codex 使用 `exec resume --last`，兩者均使用隔離設定與唯一工作目錄。做法依據 [Claude 官方續談說明](https://code.claude.com/docs/en/headless) 及 [Codex 官方非互動續談說明](https://developers.openai.com/codex/noninteractive)，並已核對本機 CLI 參數。

Claude 續談時會移走舊工具結果上的 `cache_control` 快取提示；本輪原件證明內容、編號與錯誤狀態均未變。驗證器僅在 Claude 的跨輪工具要求與結果比較中排除這個提示，其餘欄位仍逐項相符，原始請求照樣完整保存。這個欄位的用途見 [Claude 官方工具快取說明](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-use-with-prompt-caching)。新增十一項對照，本機保存驗證共 53 項通過；包括缺工具結果、改工具編號、遺失原訊息、把 assistant 文字當新指令、再次呼叫工具，以及只保留隨機值卻截掉其餘結果。

兩個 Client 都在最終標記 `FOLLOWUP:` 後多放一個空白，因此原驗收器的精確格式判準仍失敗。沒有改判準或重送來調整措辭，也沒有將完整命令改記為成功。直接重播原件，另行核對檔案全部內容、第二輪歷史及實際用量；分別移除 Client→Gateway 與 Gateway→供應商保存中的工具結果，兩個 Client 共四項缺結果對照都被拒絕。後續另證實初版驗證器會漏抓工具要求被刪的情況，已保留失敗初件並補上原工具要求、名稱及編號的完整保留檢查；新增四項實際移除工具要求對照也全被拒絕。S2 尚未全綠，不標 DONE。

Codex 原先的 `qwen3.8:27b` 路線雖執行了工具，供應商第二個 Responses 回應僅含 `reasoning`，沒有最終 `message` 或 `response.output_text.done`，Client 沒有最終輸出。這項失敗保留，未將推理內容冒充最終答案；改驗同一 Gateway 已支援的非推理模型後，有最終訊息及第二輪結果。Ollama 只支援部分 OpenAI 功能，適用邊界見 [官方相容說明](https://docs.ollama.com/api/openai-compatibility)，不能由健康頁推定每種模型的工具往返都能完成。

原件與重播：`/home/cash/.local/state/ai-gateway/r1-s2-20261109`；Claude 執行目錄 `/tmp/gateway-client-identity-50wts4hj`，Codex 程式模型目錄 `/tmp/gateway-client-identity-w4763nyl`，Codex 推理模型失敗目錄 `/tmp/gateway-client-identity-tzpvcetr`。原件與重播分開保留。工具模式不重跑 S1 已核的模型拒絕與撤權，輸出明列 `not_run`；Claude 撤權逾時仍待 S4。長串流、取消、其他故障、發布及 R2／R3 仍未由本輪驗收。

## 2026-11-09 R1-S1 同版基準

本次使用 Claude Code 2.1.319、Codex CLI 0.166.0 與本機 `qwen3.8:27b`，不呼叫付費來源。整合來源為 #108 的 3ca21da 與 main 的 68d7569；編譯版本 `r1s1-1278965c59eb` 對應整合樹 1278965c59ebd1c91b607eac054fcfdc8537f43f。完整差異與驗收原件位於 `/home/cash/.local/state/ai-gateway/r1-s1-20261109`，尚未發布。

`verify-client-identity.py` 固定核對上述 Client 版本；舊的 `verify-client-roundtrip.sh` 與歷史假供應商紀錄繼續固定 2.1.318，不能當成本次版本證據。兩種驗收不修改使用者的原 Client 設定或登入資料。

兩個 Client 使用獨立且新建的本機測試庫，名稱須以 `gateway_client_identity_validation_` 開頭；先設定 `DATABASE_URL` 指向該庫，再從整合工作樹執行：

```sh
python3 ~/tools/temp-service.py start --name gateway-r1-baseline-claude --ttl 20m --purpose 原Claude同版基準 -- env DATABASE_URL="$DATABASE_URL" CLIENT_IDENTITY_ALLOW_DATABASE_MUTATION=1 python3 scripts/verify-client-identity.py --gateway-binary /home/cash/work_space/private/ai-gateway/target/debug/agent-pool-gateway --revision r1s1-1278965c59eb --client claude --check-local --source-model qwen3.8:27b --check-capture
python3 ~/tools/temp-service.py start --name gateway-r1-baseline-codex --ttl 20m --purpose 原Codex同版基準 -- env DATABASE_URL="$DATABASE_URL" CLIENT_IDENTITY_ALLOW_DATABASE_MUTATION=1 python3 scripts/verify-client-identity.py --gateway-binary /home/cash/work_space/private/ai-gateway/target/debug/agent-pool-gateway --revision r1s1-1278965c59eb --client codex --check-local --source-model qwen3.8:27b --check-capture
```

先完成並停止第一項，再換另一個新庫執行第二項。每個 Client 成功請求上限 600 秒，拒絕請求上限 30 秒，Gateway 上限 900 秒；只送腳本內的隔離短答，預期 Claude 輸出 `W05_CLAUDE_CODE_OK`、Codex 輸出 `W06_CODEX_OK`。每個 Client 各一個成功來源請求；拒絕請求必須沒有上游嘗試。停止測試不刪除資料庫及原件。

初件兩個原 Client 都正常輸出預期文字，保存內容與供應商用量逐筆相符；舊驗證器卻把 Gateway 的保活訊息當作資料差異而停止。原件保留於 `/tmp/gateway-client-identity-h5az6qxf` 與 `/tmp/gateway-client-identity-r3_jn10s`，修正後直接重播原件，不用新模型回覆取代失敗歷史。

Gateway 只在 Client 串流加入完整的 `: keepalive\n\n`。這是 [SSE 規格](https://html.spec.whatwg.org/dev/server-sent-events.html) 定義的註解，讓連線在等待時保持活動。驗證器只排除兩側恰為 `: keepalive` 的完整區塊，其餘資料、順序、註解與分隔符仍逐字比較；原始兩側內容各自完整保存。`stream_legs_equal` 回報原始字串是否相等，`stream_events_equal` 回報排除固定保活後是否相等；不能將後者當成原始位元組相等。

新增六項對照，確認保活可插入，但改資料、刪結尾、加其他註解、改區塊內文字與缺結束分隔符仍被拒絕。相等檢查本身不證明串流完成；既有完成事件與實際用量驗證繼續執行。多輪、工具、長串流、取消、全部故障及正式環境均仍待 R1 後續步驟驗收。

## 2026-11-05 歷史假供應商基準

2026-11-05 以 Claude Code 2.1.318、Codex CLI 0.166.0，經 Gateway d7c7cfd 連本機假供應商。兩個原 Client 各自讀到固定串流並輸出預期文字；沒有呼叫正式供應商或做模型運算。舊版本紀錄保留供追溯。

## 執行方式

只用可拋棄資料庫；腳本會驗名稱及明確寫入授權，啟動時執行遷移。以現有臨時服務工具登記10分鐘期限，結束後確認服務已停止。SQLx使用已驗證的離線查詢描述編譯，執行期仍連真正隔離資料庫。

```sh
python3 ~/tools/temp-service.py start --name gateway-client-validation --ttl 10m --purpose 原Client假串流驗收 -- env SQLX_OFFLINE=true BUILD_REVISION=<目前提交> CLIENT_ROUNDTRIP_ALLOW_DATABASE_MUTATION=1 DATABASE_URL=postgres://user:password@127.0.0.1:5432/disposable bash scripts/verify-client-roundtrip.sh all
python3 ~/tools/temp-service.py list
```

成功結果為 `client roundtrip ok: all`，並列出去除內容的紀錄位置。Claude精確輸出`W05_CLAUDE_CODE_OK`，Codex精確輸出`W06_CODEX_OK`。每個Client最多45秒；結束或中斷會停止Gateway與假供應商，Gateway先複製到本次測試目錄再執行。啟動或健康檢查失敗直接結束，不重試資料庫連線。

## 保存內容

最新版為`tests/fixtures/client-roundtrip/captures/claude-code-2.1.318.json`與`codex-cli-0.166.0.json`。紀錄只保留路徑、查詢參數名稱、模型、串流模式、工具數量及類型、推理參數是否存在、必要標頭名稱，以及憑證替換與Gateway金鑰未轉送的布林結果。不保存提示、回覆、憑證、完整標頭、完整請求或使用者識別。

`check-client-captures.py`檢查允許欄位及預期值；`verify-client-roundtrip.sh`再與當次輸出逐項比對。故意把Codex最新版紀錄的`client_credential_absent`改為false，檢查確實失敗，復原後通過；證據`/tmp/gateway-client-capture-negative.log`。

## 驗證邊界

只證明這兩個版本能設定自訂入口、送出請求、讀完假串流並輸出預期文字。工具非空清單及推理參數是請求格式證據，不代表工具呼叫與結果往返完成；真實帳號、模型、帳務、保存、多人隔離、取消與未來版本均未由本測試驗證。為專注傳輸，測試Gateway明確停用內容保存，不能當作P2證據。

Codex仍對未知模型顯示本機資料不足的提示，沒有查Gateway模型清單；腳本只檢查Codex執行期間的Gateway日誌，避免Claude的請求造成誤報。Claude也會提示測試模型沒有已知上下文大小，不把提示消失當成通過條件。實測工具數量為Claude12個、Codex9個；數量會隨版本變動，不代表完整工具能力。

最終通過的執行目錄為`/home/cash/.cache/tmp/apg-client-roundtrip.nVf7X2`，含當次健康頁、Client輸出、Gateway日誌及去除內容的紀錄。最新重跑刻意在父環境開啟Bedrock與Vertex路由，Client仍僅走本機假供應商。臨時服務登記已讀回為空；另以程序完整參數檢查沒有本次Gateway及假供應商，Gateway連接埠也已關閉，證據`/tmp/gateway-client-process-closeout.json`。

## 原 Client 身分與群組隔離

`verify-client-identity.py`沿用同一假供應商，改以真正管理API建立兩位有個人API金鑰的測試使用者，以及一位尚未設定憑證的使用者。各自的模型白名單、個人憑證、群組及成員都由管理API設定，不用直接SQL繞過產品路徑。只接受本機且名稱以`gateway_client_identity_validation_`開頭的隔離庫，還須明確開啟寫入旗標；不替使用者清除既有資料。

```sh
python3 ~/tools/temp-service.py start --name gateway-client-identity --ttl 10m --purpose 原Client身分隔離驗收 -- env DATABASE_URL=postgres://user:password@127.0.0.1:5432/gateway_client_identity_validation_unique CLIENT_IDENTITY_ALLOW_DATABASE_MUTATION=1 python3 scripts/verify-client-identity.py --gateway-binary /absolute/path/to/built/gateway --revision <該執行檔版本>
```

先依目前版本建置執行檔，腳本再複製到本次測試目錄並核對健康頁版本。供應商在同一程序內只綁本機，Gateway只有本次測試環境，原Client也只收到測試設定；不使用真實供應商憑證。若隔離庫已被其他驗收初始化，會直接失敗，不能盲目清庫後重送。

預期七項為：Alice及Bob各自選對憑證、Alice不能用Bob模型、未設定憑證拒絕、撤銷個人憑證後拒絕、同帳號群組後備可用、移除成員後拒絕。正向逐筆核對本輪模型與唯一認證標頭，不能只讀最後一筆；所有Gateway金鑰及其他假供應商憑證均不得出現在請求。已撤銷的假憑證也保留在檢查及去敏集合內。負向另外直接確認403或503，再由原Client實送，確認上游沒有新增請求。

Claude對503會繼續重試；腳本30秒後停止該次Client並明列退出碼124。這能證明Gateway拒絕及沒有送到上游，不能聲稱Client已正常呈現錯誤或自動停止重試。結果只寫去內容的摘要，Client診斷先移除全部測試憑證；Gateway、假供應商及Client均須在驗收結束後確認停止。這不是第二真人、真實供應商或跨主機的證據。


2026-11-05 追加 `--client codex`，以原Codex CLI 0.166.0、自訂供應商及Responses路徑執行相同七項；預設仍為Claude。Gateway版本706e719，七項全部通過，三個成功案例各送出一次上游請求，四個拒絕案例上游均為零且Client退出碼1。結果位於`/home/cash/.cache/tmp/gateway-client-identity-88cr0em7/result.json`；兩個連接埠與Gateway程序已停止，收尾證據`/tmp/gateway-codex-identity-closeout.json`。本輪僅驗個人及群組API金鑰，不涵蓋ChatGPT的CodexFile帳號持有人綁定、工具往返、內容保存或正式部署。

CodexFile持有人另有`routes::tests::codex_owner::codex_file_owner_is_enforced_by_the_responses_route`：模型、身分、政策及請求經真正HTTP路由，供應商為本機假服務。本人成功、同租戶其他人與刪除供應商憑證檔後均拒絕，後兩者不增加上游請求。故意移除owner比對會使測試失敗。這個測試不啟動原Codex程式、不連ChatGPT，也不代表Gateway金鑰撤銷。
