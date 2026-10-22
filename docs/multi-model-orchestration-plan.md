# Claude Code 多模型任務編排計畫

## 結論

第一版不做桌面 Client，也不讓 Gateway 自動把一個請求拆成多次 Provider 呼叫。Claude Code 保持主模型與使用介面；新增一個 MCP 工具，讓 Claude Code 明確提交一組有上限的子任務。編排服務依任務種類、必要能力、隱私、成本與延遲選本地或線上模型，回傳結構化結果，再由 Claude Code 整合。

這樣沿用現有 Gateway 的身分、政策、模型資料、配額、排程與帳本，也不破壞「單一 Gateway 請求只呼叫一次 Provider」的故障與計費契約。

## 使用流程

1. 使用者照常在 Claude Code 描述複合工作。
2. Claude Code 判斷是否值得拆分；簡單工作仍由自己完成。
3. Claude Code 呼叫 `delegate_tasks`，每個子任務包含目的、輸入、預期輸出、依賴、任務分類提示、必要能力與資料敏感度。
4. 編排服務先驗證整張任務圖，再將無依賴子任務有界平行送進 Gateway。
5. Gateway 對每個子任務照常做身分、政策、能力、預算與模型選擇；任務分類由 Gateway 重新判斷，Client 提示不具權威；一次只呼叫一個 Provider。
6. MCP 回傳每項結果、實際模型、狀態、用量、成本、延遲及可重試性；不替 Claude Code 合成答案。
7. Claude Code 檢查衝突與缺漏，必要時只補派缺少的子任務，最後回覆使用者。

## 第一版範圍

### MCP 契約

`delegate_tasks` 一次接受 1–8 個子任務；預設最多 3 個同時執行、每項最多一次 Provider 呼叫，整批不得由模型自行重試。Claude Code 先依任務規模估算每項時間並給截止時間；Provider 可有最長 600 秒無輸出等待，整批截止時間依最長依賴鏈的估時加緩衝計算，不用同一個固定短上限切斷所有工作。

每項輸入：

- 穩定的子任務 ID、目的與完整上下文；
- 八類任務提示之一，以及圖片、工具、結構化輸出、推理、上下文長度等必要能力；Gateway 分類與實際請求形狀才是權威；
- `local_only`、`online_allowed` 或 `online_required`；
- 可選的依賴 ID、輸出 JSON Schema、截止時間與成本上限。

每項輸出：

- `completed`、`failed`、`cancelled` 或 `skipped_dependency`；
- 模型、Provider、分類、選擇理由、文字或結構化結果；
- token、已知成本、排隊／推論／總延遲；
- 穩定錯誤碼與「可否由 Claude Code 決定重試」。

### 編排服務責任

- 驗證 ID 唯一、依賴無循環、輸入與輸出有大小上限。
- 驗圖時拒絕 `local_only` 上游流向 `online_required` 下游，並在任何 Provider 呼叫前完成檢查。
- 將資料外傳政策隨請求送入 Gateway，由 Gateway 在 Router 前強制；`local_only` 不可只靠 MCP 選模型名稱。分類無結果、政策欄位無效或沒有合格本地模型時一律拒絕，不得退回線上原模型。
- 只排程已就緒子任務；任一依賴失敗時，後續預設跳過。
- 下游任務繼承所有上游中最嚴格的資料政策；`local_only` 任務的輸出若成為輸入，整條下游路徑保持 `local_only`。任務圖若宣告該下游為 `online_required`，驗圖階段直接拒絕；只有使用者在本次工作另行明確授權解除後，Claude Code 才能提交新的任務圖。
- 將每個子任務轉成獨立 Gateway 請求，附同一個 orchestration ID 與不同 task ID。
- Client 斷線或取消時停止未開始項目，取消進行中的 Gateway 請求。
- 回傳部分成功，不因一項失敗丟掉其他結果。
- 不保存 Prompt／Response；只把既有允許的 metadata 寫入帳本。

### 不做

- 不修改 Claude Code 執行檔、不讀取或轉送 Claude 訂閱 OAuth。
- 不要求 Claude Code 把非 Claude 模型當主模型；非 Claude 模型只經 MCP 子任務工具呼叫。
- 不做無上限代理迴圈、模型驅動重試、子任務再拆子任務或自動購買額度。
- 不讓 Gateway 自己合成最終答案；整合責任留在 Claude Code。
- 不做桌面 UI；第一版只提供 MCP、Admin 查詢與可重播測試工具。

## 實作切片

| 階段 | 交付 | 驗收 |
| --- | --- | --- |
| 1. 契約與案例 | 固定 MCP schema、錯誤碼、20 個複合案例與陰性案例；新增 Gateway 資料外傳硬政策 | 能表達平行、依賴、局部失敗、取消、隱私與預算；`local_only` 在 Gateway Router 前排除線上模型，無分類或無本地候選時 fail closed；schema 錯誤必須拒絕 |
| 2. 單項委派 | MCP 將一項子任務送入 Gateway 並回傳結構化收據 | 本地與線上模型各完成一次；原始內容不進 log／ledger；完成後先決定品質收益是否值得繼續任務圖 |
| 3. 有界任務圖 | 1–8 項、最多 3 並行、依賴與取消 | 故意造循環、逾時、依賴失敗與 Client 取消皆得到預期結果，沒有孤兒請求 |
| 4. 政策與帳務 | local／online 規則、逐項 admission、父子追蹤欄位 | 每項超額在 Provider 前拒絕；第一版不宣稱批次原子預留；每筆 Provider 用量只記一次，批次總額可對回 |
| 5. Claude Code 整體驗收 | 專案 Skill 教 Claude Code 何時拆分、何時不要拆分，以及如何整合 | 10 個複合案例與 5 個簡單案例實跑；簡單案例不得多模型化，複合案例結果可追溯 |
| 6. 旁路與上線 | 預設關閉，以明確 Skill 指令啟用；部署後讀回版本與報表 | 成功率、額外成本、端到端延遲、局部失敗率與整合正確率均有正式資料 |

## 驗收門檻

- 固定案例中，子任務切分的必要項目召回率至少 90%，不得加入會改變使用者意圖的工作。
- 簡單案例 100% 不委派；敏感且標記 `local_only` 的內容 100% 不送線上 Provider。
- `local_only` 上游接 `online_required` 下游的陰性案例必須在零 Provider 呼叫下失敗。
- 任務圖最多 8 項、依賴鏈最多 2 層、並行最多 3；子任務不能再拆子任務；故意放寬任一上限時陰性測試必須失敗。
- Provider 呼叫數等於實際開始的子任務數；沒有隱藏重試或失敗後雙重計費。
- 取消後未開始項目為零呼叫，進行中項目收到取消；服務重啟不自動重送未知狀態工作。
- 開工先凍結 30 個複合案例、必要子任務清單、禁止新增事項與逐項評分規則；正式旁路跑完同一案例集後才評估預設啟用。與單模型基準相比，依固定規則計算的整體得分不得下降，成本與 p95 延遲分開報告，不先訂假門檻。

## 程式邊界

- `agent-pool-gateway`：只補父子請求 metadata、資料外傳政策、逐項 admission 所需欄位與 Admin 查詢；不加入任務圖執行器或批次預算保留。
- `mortal-mcp`：新增 `delegate_tasks` MCP 與有界執行器，復用既有 Gateway Client，不自行持有 Provider 憑證。
- Claude Code 專案 Skill：只寫拆分規則、工具使用時機、輸出整合與失敗處理；不藏 API key。

跨 repo 合併順序：Gateway metadata／政策契約 → MCP 執行器 → Claude Code Skill → 正式旁路。每一層須能獨立關閉；MCP 不可在 Gateway 契約尚未部署時先啟用新欄位。

## 主要風險與取捨

- 多模型不保證更準：先以固定案例和單模型基準比較，不以「用了更多模型」當品質證據。
- 上下文可能在拆分時遺失：Claude Code 必須為每項提供自足輸入；MCP 不讀 Claude Code 的私有對話檔。
- 線上模型增加資料外傳面：每項明確標資料政策，預設繼承最嚴格限制。
- 平行執行提高瞬間成本：第一版逐項 admission 並限制並行；每項開始前重算剩餘預算，已開始項照常對帳，額度不足的未開始項明確跳過，不宣稱整批原子保留。
- Claude Code 是否正確使用工具不是 Gateway 可保證：以 Skill、固定案例與真實 CLI 往返驗收，不靠 prompt 宣稱。

## 開工前仍須確認

- Gateway 現有預算保留以單一請求為單位；第一版確定逐筆 admission，不先造分散式交易。
- 既有 MORTAL 工具目錄查無 Gateway 模型呼叫或取消工具；`mortal-mcp` 程式庫也未找到可直接復用的 Gateway inference client。實作前再做一次窄範圍確認，若仍沒有，才新增最小 client；取消能力必須以實際中斷連線與 Gateway 帳本驗證。
- 正式本地模型的結構化輸出與取消能力；缺能力的模型不得接需要該能力的子任務。

## 來源與證據邊界

- Claude Code 2.1.306 本機 CLI 已提供 MCP 設定與自訂 agent 入口；這證明可接工具，不證明模型會正確拆分。
- Anthropic 的 Claude Code Gateway 文件支援自架 API Gateway，但明確不保證經 Gateway 路由到非 Claude 模型。因此本方案把其他模型放在 MCP 子任務後方，不冒充 Claude Code 主模型。
- 本專案現有 Gateway 已有能力過濾、任務分類、配額、排程與帳本；規劃仍須在實作時以執行路徑與測試重新驗證，不把文件當行為證據。
