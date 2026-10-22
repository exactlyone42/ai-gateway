# Claude Code 部署後由 OAuth 退化成 401

## 一句話結論

Gateway 程式已支援 Claude 訂閱 OAuth，但部署範本沒有把 OAuth 開關與身分對應值帶進容器。2026-10-21
重新部署後，正式服務因此關閉 OAuth 路徑；Claude Code 原本有效的登入 token 被當成一般 Gateway
憑證而回 401。診斷時又誤把 Gateway token 寫進 Claude Code，短暫把它切到 API key 路徑，讓使用者
接著看到另一種 401。

## 使用者看到什麼

- 原本可用的 Claude Code 工作先以 `API Error: 499 status code (no body)` 結束；這屬前一筆長請求
  中斷事故，不是本次 401 的直接原因。
- 再輸入「繼續」後，畫面出現 `Please run /login` 與
  `401 invalid or missing API key`，並持續重試。
- 使用者沒有登出，也沒有撤銷 Claude 訂閱登入；錯誤是在 Gateway 部署後才出現。
- 診斷期間，錯誤一度從 `Invalid bearer token` 變成 `invalid or missing API key`。這不是問題自然變化，
  而是錯誤修正把 Client 認證模式改掉所造成。

## 直接原因

正式容器中沒有 `ANTHROPIC_CLIENT_OAUTH` 與 `ANTHROPIC_OAUTH_CREDENTIAL_ID`。程式因此走預設值：
OAuth 關閉、沒有對應個人身分。Claude Code 送來的訂閱 OAuth token 無法通過 Gateway 身分驗證，
所以回 401。

診斷時先看到 Claude Code 設定裡沒有 `ANTHROPIC_AUTH_TOKEN`，便誤判成「Client 少了 Gateway token」。
把 Gateway token 寫入該欄位後，Claude Code 不再送訂閱 OAuth，而改送 Gateway token；請求因而落到
API key 供應路徑，產生 `invalid or missing API key`。這個動作已撤回。

## 根因

1. 新增 OAuth 功能時，只新增程式設定，沒有同步修改 `deploy/docker-compose.yml`。程式碼能讀兩個
   新值，但正式容器根本收不到。
2. 發版驗收只確認容器健康、版本與一般請求，沒有用 Claude Code 訂閱 OAuth 做部署後真實往返。
   `/health` 與 `/ready` 都會成功，因此綠燈沒有覆蓋認證模式。
3. 沒有機械檢查比對「程式會讀的部署設定」與「容器實際轉送的設定」，同類漏值曾發生過，仍可再次
   通過 CI。
4. 診斷沒有先核對「昨天可用的正式容器」與「今天重建後容器」的環境差異，就改了 Client 設定；
   因此把伺服器部署回歸錯判為使用者端漏設。

## 已完成的解決

- 移除誤加到 Claude Code 的 `ANTHROPIC_AUTH_TOKEN`，恢復沿用 Claude 訂閱 OAuth。
- 正式環境補上 OAuth 開關與既有個人身分對應值，重新部署 Gateway。
- `deploy/docker-compose.yml` 補上兩個設定的轉送，永久修正已送 PR #94。
- 新程序透過正式 Gateway 實送後回 `OAUTH_GATEWAY_OK`；容器內確認 OAuth 開關為 true，且身分對應值存在。

## 徹底預防

1. CI 自動列出程式讀取的所有環境設定，逐項比對部署範本；少任何一項就失敗。陰性對照要故意刪掉
   OAuth 開關，確認 CI 必定變紅。
2. 部署前先展開容器設定並檢查：若正式環境預期使用 OAuth，開關不為 true、身分值空白或對應身分
   不存在，就拒絕部署。
3. 部署後除了健康檢查，固定用真實 Claude Code OAuth 經公開入口送最小請求；必須確認成功且路由到
   `anthropic-subscription`。失敗就自動回退前一版。
4. 認證錯誤診斷先分三段：Client→Gateway、Gateway 身分對應、Gateway→Anthropic。沒有證據前不得改
   Client 的認證種類，也不得用 API key workaround 取代 OAuth。
5. 401 告警須區分 Gateway 拒絕與上游拒絕，並記錄認證模式、部署版本與缺少的設定名稱；禁止記錄 token。

## 驗證與邊界

- 已證明：正式容器收到兩個 OAuth 設定；新 Claude Code 程序透過正式 Gateway 成功取得 Anthropic 回覆。
- 已證明：移除 Client 的錯誤 Gateway token 後，不再走 API key 路徑。
- 尚未證明：原本卡在 401 重試畫面的舊 Claude Code 程序能熱載入新設定；舊程序應停止後重開，因為
  認證設定可能在啟動時載入。
- PR #94 合併前，正式環境是已修復但部署範本尚未永久生效；若用舊範本再次重建，問題仍可能復發。
