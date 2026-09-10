# Admin API

## 邊界

Admin API 是 tenant-scoped control plane。它使用獨立的 Admin login credential 換取短效 Bearer
session；`x-api-key` 與資料面的 Client API Key 一律拒絕。Tenant 只來自已驗證 session，body、query
與 header 都不能指定或覆寫。

Provider、Model 與 Model Alias 可由 W10 建立、修改、啟用、停用或封存。Policy 由 W11 啟用，
Quota 由 W12 啟用；Admin API 出現 draft/disabled 資料列不代表資料面已套用。

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
| 讀取十類資源 | allow | allow | allow |
| 建立、修改、停用、撤銷、封存、輪替 | allow | allow | deny + audit |
| 列出、建立、修改 Admin account／角色 | allow | deny | deny |
| 登出自己的 session | allow | allow | allow |

規則由 `src/admin.rs` 的 `RBAC_RULES` 定義為 method × matched route × role。未列入矩陣的新端點
default-deny。最後一位 active admin 不得停用或降級；tenant 級 transaction advisory lock 保證兩位
admin 並行降級時仍至少留下一位 active admin。

## 資源端點

`{resource}` 只接受 `users`、`devices`、`clients`、`agents`、`credentials`、`providers`、
`models`、`model-aliases`、`policies`、`quotas`。

| 動作 | 端點 | 契約 |
| --- | --- | --- |
| list | `GET /admin/v1/{resource}?limit=50&after=<uuid>` | `limit` 1..100；回 `items`、`next_after` |
| get | `GET /admin/v1/{resource}/{id}` | 跨 tenant 與不存在都回 404 |
| create | `POST /admin/v1/{resource}` | typed JSON，未知欄位回 400 |
| update | `PATCH /admin/v1/{resource}/{id}` | body 必帶目前 `version` |
| delete semantics | `DELETE /admin/v1/{resource}/{id}?version=N` | identity 停用、credential 撤銷、draft 封存 |
| rotate key | `POST /admin/v1/credentials/{id}/rotate` | body 帶 `version`、1..900 秒 overlap |
| lifecycle | `POST /admin/v1/{providers|models|model-aliases|policies}/{id}/lifecycle` | Admin only；body 帶 `version` 與 `active`/`disabled` |

所有 mutable resource 使用 version CAS；stale version 回 409。User／Device／Client／Agent 是四個
獨立 Principal 維度，一把 credential 可同時指向多個維度，但至少一個；停用任一已關聯維度會在
同一 transaction 批次撤銷相關 credential，且不會因重新啟用而復活。

Credential 的完整 plaintext 只在 create／rotate 回應一次；list／get／audit 都移除 `secret_hash` 與
`fingerprint`。Provider 只接受 `env://NAME` 格式的 `credential_ref`，不接受 inline secret。
Credential-scoped policy 會綁定輪替家族；新 key 在 overlap 期間不會失去舊 key 的 hard constraints。

## 稽核與錯誤

每次成功 mutation 與 audit insert 在同一 transaction：audit 失敗時 mutation rollback。記錄包含
actor account／role、action、target、before／after、DB timestamp、source IP、peer IP、request ID。
未授權回 401、RBAC 拒絕回 403、跨 tenant／不存在回 404、CAS 或 invariant 衝突回 409、DB／認證
依賴失效回 503。所有回應沿用 gateway 的 JSON error envelope。

## 網路與設定

Admin request body 上限 1 MiB。CORS 只允許 `ADMIN_ALLOWED_ORIGIN` 的單一 exact origin；production
只接受 HTTPS，development 可用 loopback HTTP。只有連線 peer 精確命中逗號分隔的
`ADMIN_TRUSTED_PROXY_IPS` 時才採用 `X-Forwarded-For` 第一個位址，audit 永遠同時保留 source 與 peer。

`DATABASE_MAX_CONNECTIONS` 是總預算，`ADMIN_DATABASE_MAX_CONNECTIONS` 從中保留給 Admin pool，
且至少留一條資料面連線。Admin pool 飽和不會吃掉資料面 pool；控制面失敗不會中斷已進行的推論。

## 驗收

`./scripts/check.sh all` 會跑 migration、tenant/FK、bootstrap one-shot、session 撤銷、角色版本失效、
並行最後-admin 保護、十類 CRUD/CAS/audit、viewer 403、body 上限、credential cache 失效及 pool 隔離。
RBAC 陰性對照必須實際移除 middleware 檢查，確認 viewer mutation 測試變紅後再還原。
