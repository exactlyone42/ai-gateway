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
