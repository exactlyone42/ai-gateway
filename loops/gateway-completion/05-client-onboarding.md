# 05 工具連線與 Agent 邊界

目標：支援自訂端點的 IDE、CLI、Agent 容易設定 Gateway 網址與個人 Gateway 金鑰。

- 逐項核對 Claude Code、Codex、VS Code、其他可取得 Client 的官方設定與實際支援，清楚告知不支援與未知；不要求共享帳號或修改 Client 執行檔。
- Agent 自己處理本機工具、服務連線與授權；Gateway 保留工具要求、識別與結果，不能聲稱自動沿用 claude.ai 連接工具。
- Claude Code 與 Codex 自行決定何時壓縮並呼叫模型產生摘要；Gateway 不重複壓縮。此分工以 Client 是否管理對話為準，不以模型跑在本機或雲端判斷；其他 Client 預設保留其上下文，未驗證相容前不改寫。
- 自家 Agent 負責決定何時壓縮並呼叫模型；比較供應商壓縮與自行選擇摘要模型的品質、費用、速度、資料去向與模型適用條件。專用摘要模型先評估現成選項，不預設訓練；若需微調，另先確認資料、資源及驗收。
- 驗收上下文：Claude Code／Codex 各以原 Client 對官方及自架可用路線實測，確認 Gateway 不重複摘要且工具呼叫、結果、識別及順序完整；自家 Agent 使用保留的測試工作，檢查摘要是否保留目標、決策、限制及下一步，並能完成後續任務。保留原紀錄，測試摘要失敗、內容超限及跨使用者隔離；尚未實測不得宣稱通過。
- 實作前以 Claude Code 審查具體方案；核對其意見及官方來源後再執行。Claude Code 不可用時，改以官方文件與自行測試核對。壓縮會改變上下文並可能影響快取，需比較接續品質、延遲與用量，不把較短等同較好。
- 驗收：原 Client 真送、工具呼叫與結果完整往返、串流、取消、錯誤；本機與遠端 Gateway 分別驗，列出未安裝、未授權或未實測產品。

狀態 PENDING；依賴 01、02、03。

2026-10-29：以隔離 Gateway、測試資料庫與模擬供應商實測目前安裝的 Claude Code 2.1.312 和 Codex CLI 0.164.0，兩者均可用 Gateway 金鑰完成基本對話。另以 Claude Code 實測完整工具往返：供應商要求執行本機 Bash，Client 執行後將 `GATEWAY_TOOL_OK` 回傳，供應商再回最終訊息；上游收到兩次請求，使用的是測試供應商金鑰。測試使用臨時設定，全文保存關閉，未使用正式服務或真人供應商；Codex 工具往返、遠端 Gateway 與真人訂閱仍未驗證。重現探針：`/tmp/gateway-tool-check.Jze2Bh/check.py`，資料庫需先建立並依探針設定啟動。

2026-10-29：以原 Codex CLI 0.164.0、隔離 Gateway 和模擬供應商實測工具往返：供應商要求本機執行 `printf GATEWAY_CODEX_TOOL_OK`，Codex 執行後經 Gateway 回傳結果，再收到最終回覆；供應商收到兩次請求且均使用測試供應商金鑰。與 Claude Code 測試一樣，全文保存關閉，尚未驗真人供應商或遠端 Gateway。重現探針：`/tmp/gateway-tool-check.Jze2Bh/check_codex.py`，資料庫需先建立並依探針設定啟動。

2026-10-29：以目前安裝的 Claude Code 2.1.312 與使用者既有 Gateway 設定實送工具任務；Client 呼叫 Bash 執行指定的無副作用命令、收到正確工具結果，第二輪回覆指定答案，`num_turns=2` 且沒有錯誤。這是使用目前正式服務的一次實送，沒有部署或更改其設定；尚未確認該次請求的實際供應商與訂閱歸屬，也未驗第二位真實使用者。原始輸出存於僅供本機讀取的 `/tmp/gateway-real-tool.0m45sG/`。

2026-10-29：檢查目前執行環境的 PATH，未找到 VS Code、VSCodium、Cursor、Windsurf、Gemini CLI 或 OpenCode 的啟動指令；這只代表此環境無法直接啟動，不能判定它們未安裝或不支援 Gateway。這些原 Client 仍未實測。


## 2026-11-05 執行優先序

P3：以最後整合版本完成 Claude Code 與 Codex 工具要求、結果、最終答覆往返，分別記串流、取消、錯誤及帳務；模擬來源與真實供應商分開。詳見母檔與 07，本輪已啟動 goal；上述為驗收要求，尚未宣稱通過。

2026-11-05 原Client版本複核：Claude Code2.1.318、Codex0.166.0經Gateway d7c7cfd到本機假供應商，完整傳輸腳本通過，兩個精確回覆與版本化去內容紀錄相符。故意將未轉送Gateway金鑰的結果改成false，檢查實際失敗；環境注入Bedrock／Vertex路由後仍只走本機。審查抓出的Client環境繼承、無強制停止期限及不等待子程序均已修正並重跑通過。程序參數及連接埠讀回確認停止（/tmp/gateway-client-process-closeout.json），詳docs/client-roundtrip-evidence.md。這只覆蓋傳輸與請求格式，沒有開啟保存，也未驗最新版工具往返、取消、個人／群組隔離或真實供應商；既有工具探針可從本檔歷史位置查找後重用，不把舊版結果算作新版通過。

2026-11-05 原Codex CLI0.166.0經Gateway706e719的個人／群組API憑證隔離7項通過：三項成功各送上游一次，模型403及三項503皆無上游請求，Client均退出1。直接證據`/home/cash/.cache/tmp/gateway-client-identity-88cr0em7/result.json`，程序與兩個連接埠已停，見`/tmp/gateway-codex-identity-closeout.json`。不涵蓋CodexFile持有人實送、第二真人、跨主機、工具往返與保存，不標P1完成。

2026-11-05 P3開始無外部依賴驗證，P1真人／跨主機及P2正式資料缺項仍保留，不提前完成整體。原Claude2.1.318與Codex0.166.0，經a3aa441-tools測試binary到本機假供應商，各完成兩輪工具往返（Read／exec_command）。供應商首輪要求讀取隨機檔，該內容未放進prompt；第二輪必須包含同tool id、真實檔案內容及原使用者完整訊息，才回最終文字。證據`/home/cash/.cache/tmp/gateway-client-identity-aapzls0n/result.json`與`gateway-client-identity-l8v697kt/result.json`，均Client退出0且上游2次。

新增`verify-client-identity.py --check-tools`只跑工具場景，不重跑既有7項隔離；`--control-wrong-tool-result`可作錯誤內容陰性。初版審查發現只在整份JSON尋找context標記可能假綠，已改原user完整role/content相同比對；故意恢復寬鬆判準後，刪原user而把標記放工具結果的兩協定測試均失敗（`/tmp/gateway-tool-context-negative.log`）。恢復嚴格版3項測試（各2協定）通過（`/tmp/gateway-tool-fixture-tests.log`），已納入all。官方協定參考：`https://platform.claude.com/docs/en/agents-and-tools/tool-use/implement-tool-use`、`https://developers.openai.com/api/docs/guides/function-calling`。

全部未運算模型、未動正式，capture仍關閉；未覆蓋工具內容保存、取消、真人來源、遠端及自家Agent摘要，不能宣稱P3完成。臨時服務已停且測試資料庫由on-stop回收；程序／連接埠與DB的最後讀回仍待補。新版腳本全套尚未跑，尚未提交。

2026-11-05 錯誤結果原Codex陰性補強：`gateway-client-identity-bho0j0s_/tool-result.json`明確記原user仍在、real_tool_result=false及missing_real_tool_result_or_call。Client共7次上游（重試），受到30秒命令期限約束，不能誤報陰性也只有2次。正向兩個runtime的tool-result.json均2次且兩項true。收尾讀回`/tmp/gateway-original-client-tools-closeout.json`確認三個本輪測試DB為0、對應Gateway程序為空；temp-service也空。新版全套正在`/tmp/gateway-original-client-tools-all.log`（session4521），未提交；審查未見新問題，證據仍只涵蓋單次工具往返及該原user訊息，不證明全部系統上下文。

2026-11-05 工具往返新版全套`/tmp/gateway-original-client-tools-all.log`結束碼0（session4521），含3項雙協定fixture對照。原Client正向及錯結果陰性已完成；取消、工具內容保存、遠端及正式仍待驗，不勾整體完成。

2026-11-05 工具切片4bedd23首次push回Repository not found；只讀核對新舊GitHub路徑均回相同repo id1315634736，canonical為ifandonlyif42/agent-pool-gateway，管理權限ADMIN。更新本機origin為該相同儲存庫canonical SSH網址後推送成功，遠端讀回4bedd234227c088b88510ad9d59388be40a72fe4。未改正式服務。

接續取消：`--check-cancel`用不送完成事件的假串流，開始後先確認連線仍開且原Client仍執行，再向其獨立程序群組送SIGINT。Client須在5秒內結束、上游關閉，再以隔離DB核對唯一cancelled紀錄和一次attempt；查DB最多30次且SQL錯誤立即失敗，不重試連線錯誤。capture關閉。最初GNU timeout包裝導致兩Client退出137卻誤判通過；兩筆舊結果kxjy_qn_及xrejoyhg撤回為正常取消證據，只能證明程序被終止後收尾。已去除包裝器，超時或TERM/KILL僅清理且不會轉成功。

原Claude2.1.318直接SIGINT退出0，`gateway-client-identity-hu_k_2wi/result.json`：上游1、cancelled1、attempt1、usage_records2。原Codex0.166.0直接SIGINT退出1，`gateway-client-identity-4cpuv0z8/result.json`：上游1、cancelled1、attempt1、usage_records1。Codex1僅在JSON有thread.started與turn.started，且無error、turn.failed或turn.completed時接受；不是通用接受失敗碼。官方版本原始碼`https://raw.githubusercontent.com/openai/codex/rust-v0.166.0/codex-rs/exec/src/lib.rs`確認Interrupted最終exit1，`event_processor_with_jsonl_output.rs`確認中斷無完成或失敗事件；下載檔存/tmp/gateway-codex166-*。實際強殺陰性`--control-force-client-kill`已被-9拒絕，證據`gateway-client-identity-48goar1l/cancel-result.json`及對應temp-service日誌。

新版全套`/tmp/gateway-original-client-cancel-all.log`正在執行，session46386。未做取消後用量內容正確性核對，只核對紀錄數與狀態；未驗工具加密保存、取消capture、完整system上下文、跨主機及正式。測試服務均已結束，最終DB/程序讀回待補；本輪尚未提交。

取消收尾讀回`/tmp/gateway-original-client-cancel-closeout.json`：本輪cancel測試DB為0、對應Gateway程序空，temp-service空。獨立審查未見新強殺假綠，Codex中斷判準依官方0.166.0來源且結合訊號前存活、上游斷線及帳本，不單靠JSON無完成事件作結論。

2026-11-05 取消切片新版全套 `/tmp/gateway-original-client-cancel-all.log` 已結束碼0（session46386讀回）；正常取消與強殺陰性均保留。用量數值、加密保存與正式驗收仍未完成。

2026-11-05 接上原Client `--check-capture`。首次初始化因預設.cache上層群組可寫被安全門檻拒絕，改用/tmp私有目錄，未放寬保存規則。首次20秒等待不足30秒匯入週期，依startup實際週期改為最多250次、每次0.2秒就緒查詢，SQL錯誤立即失敗。寬鬆保存核對已被審查指出可同時截斷串流及丟原訊息的假綠，已改逐字比對fixture實際發出串流，解析兩輪請求確認完整原user，工具結果須在正確類型及call id欄位。嚴格版原Claude工具保存 `/tmp/gateway-client-identity-aao9d3u2/capture-result.json` 通過：2筆complete、8條內容讀回及串流逐字相符；temp-service已結束。本機dev單一見證，無模型運算、正式未動；尚未驗密文明文掃描、保存讀回變異陰性、Codex保存、取消保存及用量數值。新用量查詢已準備但未接入驗證；本輪未提交、未跑新版全套。下一步補保存陰性並依序完成其餘三個原Client場景，再接用量核對。

2026-11-05 保存與用量補驗：純驗證抽至client_capture_validation.py，截斷兩份相同串流、移除原user但marker留tool結果、錯工具結果、重複attempt、預估輸入歸零及錯實際用量均拒絕；連既有fixture共11項測試通過，已接all。Codex工具嚴格保存22ag0n5u通過。取消保存Claude49xkhoua、Codex6_1dyw4y皆incomplete且4條讀回，Claude供應商實際1/0，Codex供應商未提供用量故無provider_reported，預估另列。後續強化拒絕重複attempt與零預估，對兩筆已保存的usage-result重新執行新純驗證均過，證據/tmp/gateway-cancel-strict-usage.log，未重跑Client。新版完整工具與用量：Claude vd0npfq9、Codex v2pgkbad，各2筆complete、8條讀回、每輪實際1/1；SQL帶attempt_id且核對請求集合與唯一列數。預估僅驗有效、來源與帳本一致，未獨立證明估算準確。所有runtime均/tmp/gateway-client-identity-前綴；服務已結束。資料庫密文明文掃描、本輪新版全套、正式與跨主機仍待驗；未提交。

2026-11-05 保存驗收切片全套 `/tmp/gateway-original-client-capture-all.log` 結束碼0（session73919），包含新增11項fixture與保存/用量負向檢查。部署、正式資料、密文明文掃描及跨主機缺口維持，不標整體完成。

2026-11-05 明文殘留補驗：reject_plaintext同時掃原文及hex，新增明文／hex洩漏拒絕與缺marker拒絕，連既有共15項測試通過。原Claude工具spjsx9r1、原Codex工具g0n156s_各掃到2份spool，再掃隔離DB data-only dump與一般Gateway log，nonce、原訊息marker與回覆marker均未找到。審查指出取消原請求缺marker，已補expected_reply；另以spool檔名集合核對全部capture id，不只要求至少1檔。新版取消Claude3eqt6yq9、Codexvjwlgio6皆通過請求／回應marker、1份spool逐筆核對、DB與log掃描；同時保存未完成狀態及用量仍過。runtime皆/tmp/gateway-client-identity-前綴，plaintext-result.json有掃描數量及大小。工具兩場景在新增檔名集合判準前執行（各2檔、2capture），不得宣稱新版集合判準已在工具實送跑過。本輪服務均結束；正式未動。只能證明指定ASCII測試內容在這些位置未以原文或hex出現，不證明所有編碼或任意主機位置；fixture及Client診斷檔是刻意保存的測試證據，不屬正式保存層。新版全套待跑，尚未提交。

2026-11-05 新增檔名集合判準後，原Claude工具glfj00cx及原Codex工具0blxy45z皆重跑通過，兩份spool的capture id集合與2筆資料庫紀錄完全相符；plaintext-result.json保留capture_ids。保存、用量與明文掃描同步過。先前工具集合未實送缺口已補；本輪新版all仍在session32368，腳本未修改。

2026-11-05 明文新版all結束碼101，469過2失敗7ignored；失敗為held_token WouldBlock，已回P1修正可重現的繼承描述符持鎖問題，詳02。未拿上版綠燈取代新版結果。

2026-11-05 憑證鎖新核心099a3b8-token-unlock：原Claude工具fcbjipn8、原Codex工具9hgvzyco、原Claude取消ehqpzy9r、原Codex取消a2xts98n四場景全過；runtime在/tmp/gateway-client-identity-前綴。完整內容／逐筆spool id／DB及log明文掃描、工具實際用量1/1、取消Claude1/0及Codex無provider_reported均過；取消state=incomplete，工具各2筆complete。連同隔離兩場共六DB與Gateway程序已清空（/tmp/gateway-token-unlock-client-closeout.json）。新全套/tmp/gateway-token-unlock-all.log結束碼0；沒有正式或模型運算。原Client上游錯誤全流程仍待補，跨主機及正式缺口保留。

2026-11-05 上游錯誤驗收：af0dd5a推送已結束碼0，遠端讀回相符；新--check-error用官方400 invalid_request_error格式（https://platform.claude.com/docs/en/api/errors，下載/tmp/gateway-anthropic-error-reference.html），無模型運算。最初假設一個Client命令只送一次上游，Claude ifs1z7f0/nbo4wukd失敗；nbo4wukd/error-result.json確認6個請求、SDK retry count全0且thinking內容及body hash變動，Gateway access也有6個獨立request id，不能誤稱Gateway重試。改為有界1..6個Client請求，入口400的唯一request_id集合與帳本完全相符，每request僅1attempt。覆審要求排除總數抵銷，假上游每次錯誤加唯一序號，全部provider/client錯誤回應須與實際發出payload一對一，拒絕重複或遺漏。嚴格正向Claude tchuuwd2=6個入口/上游/完整保存、24條讀回；Codex gnjk_e2y=1個入口/上游/完整保存、4條讀回。每筆failed、無provider_reported、預估有效且來源分離，指定內容spool/DB/log無raw或hex明文，皆過。runtime均/tmp/gateway-client-identity-前綴。原Client成功SSE冒充錯誤的控制兩者均以Client exit0遭拒，見gateway-error-control-claude/codex-20261105的temp-service log；此陰性在加唯一序號前執行，但同一Client出口判準未改。連既有共20項測試過，新增包含重複錯誤不可取代遺漏、錯誤不得記成功或捏造實際usage。服務均結束；新版all待跑，未提交。範圍只HTTP400，429/5xx、連線逾時與串流途中錯誤尚未完成原Client驗收；正式、真人與跨主機仍未驗。

2026-11-05 HTTP400新版all（session78160）結束碼101，Rust468過5失敗7ignored；5項全在SQLx testing setup connect PoolTimedOut，尚未進功能斷言，詳/tmp/gateway-original-client-error-all.log。資料庫log同時有多次測試建庫引發的checkpoint同步7.997秒及10.169秒；主機負載8.19，現況DB CPU0.67%，不能只據時間認定唯一成因。不增加逾時、不略過斷言，下一次以RUST_TEST_THREADS=2降低跨測試建庫競爭，測試內部原有並行與陰性不變。

2026-11-05 低並行新版全套/tmp/gateway-original-client-error-bounded-all.log結束碼0（session77065）：RUST_TEST_THREADS=2，主Rust473過0失敗7ignored，Python備份20skipped，原5項setup逾時均過，未修改逾時與功能斷言。不能據此證明先前逾時唯一成因。HTTP400已符合原門檻泛稱的錯誤驗收；429/5xx等未驗範圍保留，不自行擴成所有錯誤矩陣的新增發布阻擋。下一優先為本人CodexFile原Client實送、壓縮接續及測試環境；第二真人、跨主機、正式備份位置／見證授權與自家Agent跨repo驗收缺口維持。
