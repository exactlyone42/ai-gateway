# 身分與 Client 憑證

## 邊界

這個模組回答「誰發的請求」，並提供給 W08 Admin API 呼叫的生命週期服務。
它不提供 Admin HTTP 端點、RBAC、audit log 或政策合併。

## 資料模型

管理服務的使用者 email 可不填；同一 tenant 的 email 不分大小寫且不得重複。修改時不提供 email 保留原值，提供 `null` 清空；列表可用 `email` 精確搜尋，忽略大小寫且只搜尋目前 tenant。建立及修改的稽核紀錄不保存 email。

`users`、`devices`、`clients`、`agents` 是四個獨立維度，彼此沒有父子外鍵。
`credentials` 用 nullable foreign key 組成一次請求的 `PrincipalContext`。每個外鍵都連同
`tenant_id` 驗證，因此無法把不同 tenant 的維度拼成一個 Principal。

`credentials` 保存：

- `type`、`display_name`、四個身分 ID 與生命週期時間。
- HMAC-SHA256 `secret_hash` 與非秘密 `hash_key_id`；HMAC key 只來自環境，不進 DB。
- SHA-256 `fingerprint` 作候選查找，及不含完整金鑰的 `key_prefix`。
- `active`、`disabled`、`revoked`、`expired` 四種狀態。

完整金鑰形狀為 `aigw_<environment>_<tenant UUID>_<32-byte random>`。建立與輪替只由
`IssuedCredential.plaintext` 回傳一次；任何讀取或狀態操作都不回傳它。

## 生命週期

| 動作 | 結果 |
| --- | --- |
| create | 產生新亂數金鑰並建立 active credential |
| disable | active → disabled，立即失效 cache |
| enable | 未撤銷、未過期的 disabled → active |
| rotate | 單一 SQL statement 鎖舊列、縮短舊 key 效期並建立唯一 successor |
| revoke | active／disabled → revoked，立即失效 cache |
| expire | 解析時把命中的到期列改成 expired；控制面亦可一次 batch 處理 tenant |

停用使用者、裝置、客戶端或代理身分時，原有行為會撤銷綁定的金鑰；重新啟用身分不會恢復已撤銷的金鑰。若只需暫時停用並恢復金鑰，應操作金鑰本身的停用與啟用。

輪替 overlap 必須介於 1 秒和 15 分鐘，同一舊 key 只能建立一個 successor。解析／cache 填入
與生命週期異動由同一把 async read/write gate 線性化，異動成功後不會再填回舊 cache。
第一版只跑單一閘道實例；W19 維持這個契約。改成多實例前必須另行補跨實例失效與 durable
counter 機制，並量測新增的資料庫成本。

## 請求解析

認證中介層先處理既有的 development bootstrap key，再解析正式 `aigw_` key：

1. 從 key 取得 environment 與 tenant，計算 fingerprint。
2. cache key 同時包含 tenant 與 fingerprint；命中後先查資料庫確認憑證及其身分仍有效，再以常數時間驗 HMAC。
3. 未命中時用一條 tenant-scoped SQL 取得 credential、更新 `last_used_at` 並組成 Principal。
4. 命中的過期列在同一個 SQL statement 轉為 `expired`；無效狀態回 401。
5. 資料庫不可用時，即使快取命中也回 503；缺少資料列指定的 HMAC key 版本也回 503。

Gateway 的 PostgreSQL connection 設 1 秒 `lock_timeout`、2 秒 `statement_timeout` 與 2 秒
pool acquire timeout。credential row 被其他 transaction 鎖住時，認證會在期限內回 503 並釋放
生命週期 gate，不會讓後續所有請求無界排隊。

快取容量由 `CREDENTIAL_CACHE_CAPACITY` 控制，TTL 由 `CREDENTIAL_CACHE_TTL_SECONDS` 控制，
且不會超過 credential 自己的 `expires_at`。`last_used_at` 是 cache TTL 粒度的近似值，不是帳務事件；
精確請求與用量由 W04 ledger 負責。

成功請求的 access log 會寫 `principal=<credential UUID>`。完整 display name 留在
`PrincipalContext`，不送 telemetry，避免姓名或自訂名稱變成 PII 外洩。

## Bootstrap

`BOOTSTRAP_CLIENT_KEY` 是可選的本機開發相容路徑，只有明確設定
`ENABLE_DEVELOPMENT_BOOTSTRAP=true` 且 `CREDENTIAL_ENVIRONMENT=dev` 才載入，仍以常數時間
SHA-256 比對且不寫 DB。其他組合若誤設，程序會拒絕啟動；未設定則 bootstrap 一律回 401。
正式 Client key 必須由 `CredentialService` 簽發；W08 交付前尚無對外 Admin API。

## HMAC key 輪替

新 credential 使用 `CREDENTIAL_HASH_KEY_ID` 與 `CREDENTIAL_HASH_KEY`。部署新 active key 時，
把仍有 credential 引用的舊 key 放入 `CREDENTIAL_RETIRING_HASH_KEYS=id=secret,...`；DB 依
`hash_key_id` 選定唯一版本後才做常數時間驗證。舊版本 credential 全部輪替或撤銷後，才能
從 retiring 集合移除該 key；過早移除會 fail closed 為 503，不會把它誤判成一般無效 key。

## 檔案地圖

| 檔案 | 責任 |
| --- | --- |
| `migrations/20260910160000_identity_credentials.sql` | 五表、tenant 外鍵與狀態約束 |
| `migrations/20260910163500_credential_hash_versions.sql` | HMAC key 版本與唯一 successor |
| `src/identity.rs` | 金鑰材料、Principal、cache 與六個生命週期動作 |
| `src/auth.rs` | Bearer 解析、bootstrap 相容與錯誤映射 |
| `src/middleware.rs` | Principal 注入 request／response，提供 access log 使用 |
| `src/config.rs` | active／retiring HMAC keys、environment、cache 容量與 TTL |

## 驗收

`./scripts/check.sh all` 會啟動一個有界的本機 PostgreSQL，讓 `sqlx::test` 在隔離資料庫套用真
migration。測試涵蓋跨 tenant 外鍵、狀態轉移、單一 successor、HMAC key overlap、DB 故障、
cache／撤銷競態、production bootstrap 拒絕，以及五表明文掃描的陰性與注入正對照。

## 群組與權杖歸屬（2026-11-01 決定）

同一個人的多個帳號（不同 email、角色）各是一個 user，放進同一個群組；供應商權杖綁在群組，查找順序是使用者自己的、再所屬群組的。放棄的做法：在 user 之上新增「人」，或讓一個 user 掛多個 email，因為都更複雜。群組不檢查成員是否同一個人，把不同人放進掛有權杖的群組就是共用訂閱席次，由管理者負責。


2026-11-05 群組憑證實作（尚待完整發布驗收）：啟用個人憑證目錄後，讀取先找使用者；只有確定未設定才找群組，損壞、檔案鎖忙或資料庫錯誤均拒絕。符合的群組必須只有一個，訂閱與 API 金鑰分開綁定；同一使用者與群組即使識別碼相同，檔案路徑也不同。資料庫只存完整 SHA-256 摘要，不存秘密；實際使用前比對正規化後的權杖，避免檔案更新但資料庫未提交時誤用新憑證。

管理者可對 `/admin/v1/groups/{group}/provider-credentials/{provider}/{kind}` 使用 GET、PUT、DELETE；`kind` 為 `subscription` 或 `api`，PUT 內容為 `{"token":"本人憑證"}`。provider 必須是同租戶已啟用的供應商，訂閱限 Anthropic；Codex 仍沿用明確指定的一位持有人，不走群組後備。GET 只回設定、啟用與檔案是否符合綁定的狀態，不回憑證。Operator、Viewer 均不可操作這個介面。

每次變更先記操作嘗試，再鎖定群組、原子更新檔案，最後把綁定與成功稽核一起提交。檔案與資料庫無法一起回滾；失敗時回結果未確認，之後應先 GET 查狀態，再明確更新或撤銷。讀取若發現檔案與綁定不符會拒絕，不自行猜測要用哪一版。資料庫取鎖最多等2秒，單次查詢最多5秒。

群組請求保留會員列、內容綁定及檔案共享鎖到上游回應標頭；移除會員另外取得同一使用者的本機排他鎖，避免資料庫連線失效後提前完成撤權。API 與訂閱共用這把會員鎖，憑證檔仍分開。未配置個人憑證目錄的入口，若群組已有啟用的憑證綁定，會拒絕移除會員；查驗前先鎖群組，再以新的查詢讀取綁定。這依賴 Gateway 程序使用相同的個人憑證目錄；跨主機鎖、直接修改資料庫、多人真實訂閱與正式上線尚未驗收。以群組掛憑證不會自動判定這些使用者是否同一個人，原有管理者責任不變。
