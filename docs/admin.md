# Admin API

## 邊界

Admin API 是 tenant-scoped control plane。它使用獨立的 Admin login credential 換取短效 Bearer
session；`x-api-key` 與資料面的 Client API Key 一律拒絕。Tenant 只來自已驗證 session，body、query
與 header 都不能指定或覆寫。

Provider、Model 與 Model Alias 可由 W10 建立、修改、啟用、停用或封存。Policy 由 W11 啟用，
Quota 由 W12 啟用；Admin API 出現 draft/disabled 資料列不代表資料面已套用。
Models 畫面另顯示用途、能力旗標與最近 30 天的實際成功率及平均延遲；啟用中的模型也能修改
這些路由資料。`quality_tier` 只保留作舊 Client 的相容別名，不代表跨用途的絕對品質。

## 認證與 bootstrap

| 動作 | 端點 | 認證 |
| --- | --- | --- |
| 建立第一位 admin | `POST /admin/v1/bootstrap` | `Authorization: Bearer <ADMIN_BOOTSTRAP_TOKEN>` |
| 建立 session | `POST /admin/v1/sessions` | `Authorization: Bearer <admin login token>` |
| 登出 | `DELETE /admin/v1/session` | `Authorization: Bearer <short session token>` |

Bootstrap token 必須和 `ADMIN_BOOTSTRAP_TENANT_ID` 一起設定、至少 32 bytes，且對指定 tenant 永久
one-shot；成功後即使重啟也不能再用。Bootstrap 與新 admin account 回傳的 login token 都只顯示一次。
DB 只存 domain-separated HMAC、fingerprint 與非秘密 key id。Session 絕對期限為 60 到 3600 秒，
每次請求都重查 account、login credential、版本與狀態，因此 logout、撤銷、停用或降權立即生效。

## RBAC

| 操作 | admin | operator | viewer |
| --- | --- | --- | --- |
| 讀取十二類資源 | allow | allow | allow |
| 建立、修改、停用、撤銷、封存、輪替 | allow | allow | deny + audit |
| 列出、建立、修改 Admin account／角色 | allow | deny | deny |
| 登出自己的 session | allow | allow | allow |

規則由 `src/admin.rs` 的 `RBAC_RULES` 定義為 method × matched route × role。未列入矩陣的新端點
default-deny。最後一位 active admin 不得停用或降級；tenant 級 transaction advisory lock 保證兩位
admin 並行降級時仍至少留下一位 active admin。

## 資源端點

`{resource}` 只接受 `users`、`devices`、`clients`、`agents`、`credentials`、`providers`、
`models`、`model-aliases`、`policies`、`quotas`。
另接受 `model-task-profiles` 與 `task-classifier-configs`；前者保存模型適合的任務、信心、來源與人工覆寫，
後者保存分類器模型、taxonomy、模式、最低信心及逾時，兩者都沿用 tenant 隔離、版本 CAS 與既有 audit。

| 動作 | 端點 | 契約 |
| --- | --- | --- |
| list | `GET /admin/v1/{resource}?limit=50&after=<uuid>` | `limit` 1..100；回 `items`、`next_after` |
| get | `GET /admin/v1/{resource}/{id}` | 跨 tenant 與不存在都回 404 |
| create | `POST /admin/v1/{resource}` | typed JSON，未知欄位回 400 |
| update | `PATCH /admin/v1/{resource}/{id}` | body 必帶目前 `version` |
| delete semantics | `DELETE /admin/v1/{resource}/{id}?version=N` | identity 停用、credential 撤銷、draft 封存 |
| rotate key | `POST /admin/v1/credentials/{id}/rotate` | body 帶 `version`、1..900 秒 overlap |
| lifecycle | `POST /admin/v1/{providers|models|model-aliases|model-task-profiles|policies|quotas}/{id}/lifecycle` | Admin only；body 帶 `version` 與 `active`/`disabled` |

所有 mutable resource 使用 version CAS；stale version 回 409。User／Device／Client／Agent 是四個
獨立 Principal 維度，一把 credential 可同時指向多個維度，但至少一個；停用任一已關聯維度會在
同一 transaction 批次撤銷相關 credential，且不會因重新啟用而復活。

Credential 的完整 plaintext 只在 create／rotate 回應一次；list／get／audit 都移除 `secret_hash` 與
`fingerprint`。Provider 只接受 `env://NAME` 格式的 `credential_ref`，不接受 inline secret。
Credential-scoped policy 會綁定輪替家族；新 key 在 overlap 期間不會失去舊 key 的 hard constraints。
Credential-scoped quota 使用相同輪替家族正規化。Governance report 回傳目前最多 100 個 active
quota window 的 quota、limit kind、current、maximum 與 UTC reset time；Request detail 保留當次
immutable quota decision。事故重建可在同一 tenant lock 下執行
`SELECT recompute_quota_windows('<tenant-uuid>')`，其來源是不可變 reservation 與 reconciliation event。

舊模型轉換只建立 `draft`、零分、零信心的未分類 placeholder，不會參與路由。Operator 可建立與修改
profile 或設定 shadow 模式；只有 Admin 能啟用 profile 或把分類器改成 `enforced`。`migration` 來源不能由
Admin API 偽造；要啟用舊資料必須改成有來源的人工、供應商或 benchmark 設定。

## 稽核與錯誤

每次成功 mutation 與 audit insert 在同一 transaction：audit 失敗時 mutation rollback。記錄包含
actor account／role、action、target、before／after、DB timestamp、source IP、peer IP、request ID。
未授權回 401、RBAC 拒絕回 403、跨 tenant／不存在回 404、CAS 或 invariant 衝突回 409、DB／認證
依賴失效回 503。所有回應沿用 gateway 的 JSON error envelope。

## 事故記錄

事故屬於登入 session 的 tenant；`system` 只表示影響該 tenant 的共用 Gateway，不能跨 tenant
讀寫。清單可用狀態、發生日期、服務與影響範圍篩選。事故目前值包含標題、狀態、服務、影響範圍、
開始／結束時間、摘要、影響、根因、暫時處理、永久修正、預防措施、驗證證據與版本。狀態固定為
`investigating`、`mitigated`、`resolved`、`closed`。

建立事故、變更狀態或追加時間線都必須在同一 transaction 寫入事故目前值、不可修改的事件及
既有 audit。事件只追加，資料庫拒絕 update、delete 與 truncate；事故目前值用 version CAS 防止
兩位管理者互相覆蓋。事件保存 actor、DB 時間、類型、改前／改後與安全說明。請求關聯只接受同
tenant 的 request UUID；部署關聯只存版本識別字，不複製請求或回應內容。

事故端點沿用 Admin session；admin 與 operator 可寫，viewer 只讀，Client API Key 一律 401。
所有文字欄位在寫入前拒絕疑似 Gateway／Provider 金鑰、Bearer token、完整 Prompt 或 Response；
拒絕時整個 transaction 不得留下事故、事件或 audit。輸入限制是防止意外保存，不取代日誌與
回應的既有去敏檢查。

## 網路與設定

Admin request body 上限 1 MiB。CORS 只允許 `ADMIN_ALLOWED_ORIGIN` 的單一 exact origin；production
只接受 HTTPS，development 可用 loopback HTTP。只有連線 peer 精確命中逗號分隔的
`ADMIN_TRUSTED_PROXY_IPS` 時才採用 `X-Forwarded-For` 第一個位址，audit 永遠同時保留 source 與 peer。

`DATABASE_MAX_CONNECTIONS` 是總預算，`ADMIN_DATABASE_MAX_CONNECTIONS` 從中保留給 Admin pool，
且至少留兩條資料面連線：一條持有 W12 單實例鎖，一條服務流量。Admin pool 飽和不會吃掉資料面
pool；控制面失敗不會中斷已進行的推論。

## 驗收

`./scripts/check.sh all` 會跑 migration、tenant/FK、bootstrap one-shot、session 撤銷、角色版本失效、
並行最後-admin 保護、十二類 CRUD/CAS/audit、viewer 403、body 上限、credential cache 失效及 pool 隔離。
RBAC 陰性對照必須實際移除 middleware 檢查，確認 viewer mutation 測試變紅後再還原。
