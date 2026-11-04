# LOOP: codex-held-auth

| 欄位 | 值 |
| --- | --- |
| 狀態 | 已完成 |
| 優先度 | P1 |
| 前置 | 無 |
| 開始 | 2026-11-04T22:45+08:00 |
| 結束 | 2026-11-04T23:12+08:00 |
| Token | 未取得 |

## 目的
Gateway 在運行中讀取個人 Codex 訂閱憑證，缺少憑證不影響啟動。
## 完成判準
- [x] 缺檔、稍後出現、原子替換、到期、錯誤帳號、非法格式皆有測試。
- [x] 不回傳或記錄權杖；訂閱失敗不改走 API。
- [x] 全套檢查、隔離服務與原 Codex 實送通過。
- [x] 正式部署及正式 Codex 實送通過。
## 已知
基準 origin/develop=d1c29e8。Codex 使用 auth.json，官方 CLI 負責更新。
## 已做
- 建立隔離工作樹，保留其他工作樹修改。
- 正式部署完成；Essence 預設 Codex 與目前 Orca 帳號設定已改用 Gateway，新程序實送成功。Mac 設定未動。
- 動態讀取與上游憑證隔離測試通過；原 Codex 經隔離 Gateway 回答 OK。
- 同步工具不複製更新權杖；主機 10 秒同步計時器已啟用，正式 Gateway 尚未切換。
## 已確定不可行
- 不將 Codex 登入憑證當成一年期 Claude 權杖。
## 證據
- 全套：/tmp/gateway-codex-check-all-complete.log，Rust 320、前端 2、Python 23、守門自測 32。
- 原 Codex：/tmp/gateway-codex-client-result.log，回答 OK。
- 不重啟撤去憑證為 503，補入後為 200，/tmp/codex-runtime-restored.txt 含完成事件。
- PR #109 已合併；公開版本 63fe6d0bef32。正式原 Codex 回答 OK，資料庫 selected_provider=codex-subscription、status=completed、jev_attempted=false。
- 正式收據：/home/cash/.local/state/gateway-deployments/codex-held-auth-63fe6d0bef32/accepted.json；Claude 實送與跨切換串流通過。
- 日誌未出現 Gateway 金鑰、訂閱權杖或測試 prompt。
- 部署資源期限標記新增 1 項測試；Python 共 24 項。
## 來源
- https://learn.chatgpt.com/docs/auth ，2026-11-04，登入保存及更新。
## Trade-off
由官方 Codex 更新憑證，Gateway 唯讀取得；避免兩個程序同時更新。
## 下一步
無待辦；官方 Codex 仍負責更新登入，Gateway 不自行續期。
