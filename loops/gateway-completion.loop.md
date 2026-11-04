# LOOP: gateway-completion

> 既有總工作，目標依使用者 2026-10-29 指示擴成兩張 Gateway 架構圖的全部差距。
> 工作樹 `feature/gateway-completion`；每次只展開一個子檔，依順序執行；舊總檔完整保存於 [既有工作](gateway-completion/previous-scope.md)。

| 欄位 | 值 |
| --- | --- |
| 狀態 | RUNNING |
| 優先度 | P0 |
| 開始 | 2026-10-25；目標擴充 2026-10-29 |
| 結束 | — |

## 目的
完成現況圖到目標圖的所有差距，讓使用者以自己的帳號安全使用各模型，並以實際 Client 完整驗收。

## 完成判準
- [ ] 子檔全部有實作、測試、陰性對照及整體驗收證據。
- [ ] 整合測試、部署、公開版本與原 Client 實送各自通過；每個來源和未驗項逐項說明。
- [ ] 多人使用各自權杖、遠端 Gateway、加密保存、工具往返與原 Client 驗收均有直接證據。
- [ ] Claude Code／Codex 管理自身對話時，Gateway 不重複摘要；自家 Agent 的摘要決策、摘要方式評估及接續驗收有直接證據。

## 已知
- 主工作樹兩圖：`/home/cash/work_space/private/agent-pool-gateway/docs/agent-pool-gateway-現況.pdf` 與 `agent-pool-gateway-目標.pdf`。圖不等於實作證據。
- 正式版 2026-10-29 為 d1c29e89990d；一份 Anthropic 權杖，已用 Claude Code 實送。
- 既有總工作、identity-groups 與 content-capture 工作樹已存在；保留未提交修改。遠端整合分支是 `develop`；實際合併前重查。
- 2026-10-31 決定：Claude Code／Codex 自行管理壓縮；自家 Agent 決定何時壓縮並呼叫模型，供應商功能或自行選擇的摘要模型均待評估。目標圖已更新，未代表功能實作。

## 已做
- 2026-10-29 使用者指定以母子檔管理整體目標；已列出子任務與已知證據，尚未完成整體驗收。
- 2026-10-29 已停用每五分鐘排程，改由本次 goal 接續；排程唯一一次執行已結束，逐項對照仍未完成。
- 未交付理由：單機隔離環境已驗入帳衝突處理、加密備份還原與單一寫入者；正式 Gateway 使用的資料庫與磁碟卷皆不同於舊版備份預設，來源競爭保護、正式備份整合、損壞檔人工恢復、獨立主機與原 Client 驗收仍未完成。尚未部署，也無正式交付收據。
- 目前工作樹的加密備份入口會檢查檔案並寫驗證紀錄，但其協調鎖只約束遵守鎖的程式，不能阻止其他資料庫連線寫入；該紀錄不能證明資料庫與入帳檔同一時間點，也不是正式交付收據。正式 Gateway 的資料庫帳號目前具超級使用者權限；權限分離須先在隔離環境完整驗證。

## 已確定不可行
- 不使用健康頁、文件或測試替代原 Client 實送；不把同一權杖當成多人隔離。

## 證據
- 正式 `/health` 讀回；先前實送與切換證據在 `~/.local/state/gateway-held-token-cutover-20261029/`。

## 來源
- 兩張圖、`docs/architecture.md`、`docs/request-response-storage.md`、既有工作樹及各子檔。

## 下一步
使用者 2026-11-02 指示重新接續；保留所有工作樹與未提交修改。
1. [01 對照現況](gateway-completion/01-baseline.md) — 已列對照；實際驗收由各子任務補證。
2. [02 身分與個人憑證](gateway-completion/02-identity-credentials.md) — 實作與單人實送已驗，第二位真實訂閱者、跨主機及完整安全驗收未完成，留待整合驗收。
3. [03 模型與供應商](gateway-completion/03-model-providers.md) — 已驗證本人 Anthropic、本機 Ollama 與模擬來源；其他真實帳號、vLLM、工具與正式驗收仍未完成，留待整合驗收。
4. [04 加密保存](gateway-completion/04-encrypted-records.md) — RUNNING；使用者同意暫時不要求 Claude 審查，接續備份來源排他及異常恢復的隔離整合與驗收。
5. [05 Client 與 Agent](gateway-completion/05-client-onboarding.md) — PENDING；涵蓋上下文分工、摘要評估及原 Client 驗收。
6. [06 整合與交付](gateway-completion/06-delivery.md) — PENDING。

每次只讀母檔、當前子檔與必要來源；缺少他人帳號或跨主機等外部驗收時，保留未完成狀態並在整合階段補證，不以模擬測試替代。不得跳過失敗檢查。
