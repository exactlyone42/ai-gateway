# 01 對照兩圖與現況

目標：逐項建立現況、目標、程式及正式環境的對照，不把圖或文件視為行為證據。

- 比對使用者指定兩張 PDF、圖的 JSON 原稿、正式版本、`origin/main`、工作樹、未合併 PR 與既有任務。
- 每一項標示已驗證、部分、未實作、待驗證；記錄實際程式位置、測試與證據，釐清目標圖沒有畫出的必要流程。
- 保留 `feature/content-capture`、`feature/identity-groups`、`feature/gateway-completion` 的工作，不覆蓋未提交資料、不重建同一工作。
- 驗收：兩張圖每個方塊與連線都有對照、負責子檔與驗收；缺少證據不得標完成。

已知：2026-10-29 公開版本 d1c29e89990d；兩圖及原稿位於主工作樹 docs。此子檔 RUNNING；下一步核對實際程式與既有工作。

初步核對（2026-10-29）：正式版本路由含 Messages、Responses、Chat Completions 與模型清單；程式在 `state.rs` 建立一份持有的 Anthropic 權杖，在 `adapters/provider.rs` 準備上游請求時取用。這證明有一份權杖的程式路徑，不證明多人隔離或其他供應商實送。兩張圖核對尚未完成。

本輪讀取兩張 PDF 文字：目標另列個人憑證、其他供應商、本機模型、加密保存與工具往返；現況圖只描述 Anthropic 與單份權杖。這只是圖上差異，尚須逐項查程式與實測。既有 `gateway-completion` 排程啟用、每五分鐘執行；本地與遠端已有同名分支，未查到同名未合併 PR。

既有工作樹 `feature/gateway-completion` 有未提交的 `docs/gateway-completion-plan.md`、`docs/routing.md` 及另一個 loop；不覆蓋。`feature/content-capture` 乾淨但落後 main，是否已有內容保存須繼續查程式及歷史。

2026-10-29 改由本次 goal 接手：排程已停用，執行紀錄只有一次且已結束。`origin/main` 與此前線上版本均為 `d1c29e89990d`；此比對不代表本輪重新驗過線上版本。

已讀該版本程式：`src/state.rs` 有 OpenAI、Anthropic、Gemini、xAI、vLLM 和 Ollama 的供應商設定；`src/adapters/provider.rs` 有各供應商請求轉接與憑證欄位。這證明程式存在路徑，不能證明每家已設定、可用或完成真實請求。`src/services/request_quota.rs` 有模型政策、供應商選擇及額度處理；`src/routes/messages.rs` 有組裝請求與記帳流程，完整資料流與實測仍待核對。加密內容保存不可由 `capture` 字樣推定：目前找到的多是測試日誌擷取，需繼續查實作和分支歷史。

直接程式證據：`src/state.rs:98` 只建立一個 `HeldToken`；`src/held_token.rs` 讀一個檔案到記憶體，管理 API 更新同一個檔案與記憶體；`src/routes/messages.rs:202` 組裝上游請求時，`src/adapters/provider.rs:296` 取得當下權杖並加入 Authorization。`src/model_registry.rs:696` 只按租戶、憑證參照及供應商資訊選設定，未按使用者選取個人上游憑證；多人各自使用的目標交給 02，不能把租戶限制當作使用者隔離。

`feature/identity-groups` 相對 main 的已提交差異只有任務紀錄，主工作樹仍有未提交修改，需另查；`feature/content-capture` 相對 main 沒有獨有提交，不能視為已實作保存。未合併 PR 為 #106 和 #94，標題均非整體目標；需查其具體變更與是否相關。下一步完成兩圖每個方塊及連線的證據表，再進 02。

## 兩圖逐項對照（程式版本 d1c29e89990d）

以下「部分」只表示找到相關程式，尚未以本輪測試或正式請求確認完整行為。驗收由所列子任務完成。

| 現況／目標方塊 | 程式及差距 | 子任務 |
| --- | --- | --- |
| Claude／Agent、IDE／CLI／Agent | Gateway 入口在 `src/routes.rs`；Agent 本機執行工具和不同 Client 的端到端往返待實測。 | 05、06 |
| 入口與身分、身分與連線 | `src/identity.rs` 有 Gateway 金鑰與使用者、設備關聯；遠端使用與完整隔離待驗。 | 02、05 |
| 請求政策、政策與額度 | `src/services/request_policy.rs`、`request_quota.rs` 有政策與額度處理；各身分的實際隔離待驗。 | 02、06 |
| 模型與額度、模型選擇 | `src/model_registry.rs`、`src/services/request_quota.rs` 有模型候選與來源選擇；逐供應商路由與格式轉換待驗。 | 03 |
| 組裝上游請求、憑證選擇 | `src/routes/messages.rs:202` 與 `src/adapters/provider.rs:296` 取供應商設定並加上憑證；目前沒有按使用者選取個人憑證。 | 02 |
| 記帳與送出、供應商轉接 | `src/routes/messages.rs:217` 預留記帳，`:271` 記錄嘗試，`:287` 送出；串流與工具往返須實測。 | 03、06 |
| Anthropic | 程式支援訂閱或 API 金鑰；先前 Claude Code 已實送單一訂閱，本輪未重跑，多人待驗。 | 02、03、06 |
| 記憶體中的權杖、主機權杖檔 | `src/held_token.rs` 讀單一檔案並保留單一權杖；管理 API 更新該份權杖。 | 02 |
| 目前限制、憑證保存 | `src/model_registry.rs:696` 只按租戶等資訊選設定，未按使用者；需補個人隔離、更新、撤銷與稽核。 | 02 |
| 用量與稽核 | `src/ledger.rs` 與 `src/routes/messages.rs` 有記帳路徑；身分歸屬、撤銷與操作稽核的完整結果待驗。 | 02、06 |
| 加密保存層 | 已查正式版與保存分支的檔案和提交，尚未找到請求／回應的專用加密保存實作；先寫後送、權限、期限、刪除待實作並驗證。 | 04 |
| OpenAI／Gemini／xAI | `src/state.rs` 和 `src/adapters/provider.rs` 有各自設定與轉接；未證明每家已設定或真實可用。 | 03 |
| 本機／自架模型 | `src/state.rs` 和 `src/adapters/provider.rs` 有 Ollama／vLLM 設定與轉接；實際連線與工具能力待驗。 | 03 |

兩圖連線也需驗證：Client↔入口↔政策↔選模↔憑證↔送出↔來源，以及權杖檔→記憶體→上游請求，程式路徑如表；目標的憑證保存→選擇尚缺按人隔離，政策→用量與選模→加密保存需驗完整請求及失敗情境。所有雙向箭頭的回應、串流與工具往返都要以實際 Client 驗收，不能僅依程式存在判定通過。

未合併 #106 改動來源選擇、路由記錄與後台；#94 只改部署設定，均未涵蓋個人憑證或加密保存。主工作樹的架構圖、保存設計與術語文件尚未提交，必須保留並作為後續子任務的需求來源。

狀態：圖上方塊及連線已建立初步對照和負責子任務；各路徑只有程式證據，尚無本輪端到端驗收。下一步進 02，回收實測證據並更新相應子檔。

`./scripts/check.sh loops` 目前被既有未提交 `loops/agent-platform-decisions.loop.md` 不符合格式擋下；未修改或繞過該檢查，需查原工作所有權與正確處理方式。
