# 使用自己的 Codex 訂閱

Gateway 只讀取登入後的存取權杖，不替使用者登入，不改用 API 計費。登入及到期更新仍由官方 Codex 負責；沒有可用憑證時回傳 503，不重送請求。

設定 `CODEX_AUTH_FILE` 為 Gateway 可讀取的登入檔，`CODEX_ACCOUNT_ID` 為指定帳號。即使檔案尚不存在，服務仍能啟動；每次準備訂閱請求時重新開檔，稍後新增、原子替換或移除都會生效。檔案只允許擁有者讀寫，上限 64 KiB，且須為未到期的 ChatGPT 登入憑證。固定帳號避免登入檔被換成另一個人的帳號。

容器請掛載專用目錄，而不是單一檔案或整個 Codex 設定目錄。用 `scripts/sync_codex_auth.py --source <個人登入檔> --destination <專用目錄>/auth.json --uid <容器使用者 UID>` 同步；僅複製存取權杖與帳號識別，不複製更新權杖。來源缺失或無法解析時寫入不可用狀態，避免繼續使用舊資料。由主機排程定期執行，容器唯讀掛載該目錄。官方 Codex 沒有持續更新登入時，這條路會在存取權杖到期後拒絕服務，不能宣稱 Gateway 已具備自動登入或自動更新能力。

模型登錄的 provider key 使用 `codex-subscription`，credential ref 使用 `env://CODEX_AUTH_FILE`，base URL 使用 `https://chatgpt.com/backend-api/codex`，operation 僅限 `openai_responses`。Client 使用獨立的訂閱模型名稱，政策只允許這個名稱，避免路由選到 API 模型。Codex 只帶 Gateway 金鑰，以自訂 provider 呼叫 Gateway 的 `/v1/responses`；Gateway 補上伺服器保管的權杖與帳號，向訂閱端點的 `/responses` 送出。

驗收需分別確認缺檔、補檔、替換、到期、帳號不符、權限過寬、原 Client 實送及正式版本。單元測試不證明正式訂閱額度可用。

來源：https://learn.chatgpt.com/docs/auth 、https://learn.chatgpt.com/docs/enterprise/connect-to-a-gateway ，查證 2026-11-04。

主機同步服務使用 deploy/gateway-codex-auth-sync.service 與同名 timer，每 10 秒執行一次。程式安裝在 root 擁有的 /usr/local/lib/agent-pool-gateway/，來源路徑放在 /etc/agent-pool-gateway/codex-auth-sync.conf；專用輸出目錄為 /var/lib/agent-pool-gateway/codex-auth，容器使用者 UID 10001，目錄唯讀掛載到 /run/gateway-codex。同步服務只更新憑證快照，不會呼叫模型。

當候選包含 Codex 訂閱時，只保留訂閱候選，略過任務分類與自動選模型，避免明確選擇訂閱後又被改成其他供應商。沒有可用訂閱憑證時仍回傳 503，不改走 API。
