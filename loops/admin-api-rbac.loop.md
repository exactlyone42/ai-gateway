# LOOP: admin-api-rbac

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#16](https://github.com/DnaBoss/agent-pool-gateway/issues/16)（RBAC 部分）＋ [#8](https://github.com/DnaBoss/agent-pool-gateway/issues/8)｜分支 `feature/admin-api-rbac`｜段落 `W08`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 已交付 |
| 優先度 | P1 |
| 前置 | `identity-credential`, `usage-ledger` |
| 開始 | `2026-09-11T01:54+08:00` |
| 結束 | `2026-09-11T03:11+08:00` |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
後台的權限在 API 層擋得住，不是靠前端藏按鈕；每個控制面操作都留得下「誰做了什麼、改前改後」。

## 完成判準
- [x] Admin 認證與 Client API Key **兩套語意分開**：Admin 走短效 access token／安全 session，
      Client 走 API Key。兩者不共用驗證路徑。
- [x] 三角色 `admin`／`operator`／`viewer`，權限邊界照規格 §15。
- [x] Tenant 一律取自已驗證的 Admin session；request body／query／header 不能覆寫，跨 tenant
      list/get/mutate 不洩漏存在性，關聯由複合 FK 再擋一層。
- [x] 權限是 machine-readable、default-deny 的 method × route/action × role 矩陣；新端點沒列入就失敗。
- [x] RBAC 在 **API layer** 強制，不是 UI 層。
- [x] 控制面 CRUD：User／Device／Client／Agent／Credential／Provider／Model／Policy／Quota。
- [x] audit log 欄位齊全：actor／action／target／before／after／timestamp／source IP／request_id。
      覆蓋 credential create/revoke/rotate、draft 資源 update/archive、政策／配額更新、RBAC 變更；
      Provider／Model 啟停由 W10、Policy 由 W11、Quota 由 W12 實作時沿用此 transaction audit 契約。
- [x] Admin API 與推論 API 的權限完全分離；CORS 從嚴。
- [x] Bootstrap 對設定指定的 tenant 永久 one-shot；並行只能一個成功。Session 只存 hash，
      絕對期限、logout／撤銷／帳號停用／角色降級都在下一次請求立即生效。
- [x] 所有 mutable resource 用 version CAS；list 強制有界分頁；Admin DB pool 與資料面 pool 分開。
- [x] Admin 掛掉不得影響進行中的推論（規格 §63）。
- [x] **陰性對照 1**：`viewer` 憑證直接打 `admin` 端點（繞過 UI）→ 必須 403 並留 audit。
- [x] **陰性對照 2**：把 RBAC 檢查從某個端點移除 → 對應測試必須變紅。

## 已知
- 現況沒有任何 Admin API，也沒有角色概念。
- 規格 §14／§15 明確：不共用 credential semantics；RBAC 必須 API 層強制。
- 第一版**不做** SSO 與四眼核准流程（既有卡 #8 的範圍縮小，見 `docs/work-breakdown.md`）。
- 三角色是 W08 已核定契約：`admin` 包含 Platform owner／Organization admin，`operator` 對應
  Team manager，`viewer` 對應 Auditor；User/service 是資料面 caller，不可登入 Admin。第一版
  不做跨 tenant global admin；所有角色只在自己的 tenant 生效。
- 查重：既有 loop／卡接手；遠端沒有 `feature/admin-api-rbac`、沒有開放 PR，不另開卡。
- `audit_logs` 已 append-only，但缺 actor admin／role、source IP、request_id；W08 migration 補欄，
  所有控制面 mutation 與 audit insert 必須在同一 transaction，audit 失敗就 rollback。
- Client credential 保持 `credentials`／`require_client_key`；Admin 另用 `admin_accounts`、
  `admin_login_credentials`、短效 `admin_sessions` 與獨立 middleware，不接受 Client API Key。
- 第一位 admin 由顯式 bootstrap secret 建立；登入 credential 只換短效 session，plaintext 都只回一次。
- Bootstrap secret 綁定 `ADMIN_BOOTSTRAP_TENANT_ID`，成功與 audit 同 transaction 後永久失效；
  禁止停用最後一位有效 admin。Admin token 採 Bearer，不使用 cookie／query string。
- User／Device／Client／Agent 受 ledger FK 保留，不做硬刪；停用時用一條 set-based SQL 撤銷
  關聯 credentials 並清 tenant cache，重新啟用也不恢復舊 key。Credential 是撤銷。
- W08 擁有九類資源的控制面 CRUD schema；Provider 只存受限格式的 credential reference，不收
  inline secret。Model／Provider／Policy／Quota 在其執行段落前只允許 `draft`，不得宣稱已生效；
  W10 啟用 Model／Provider，W11 啟用 Policy，W12 啟用 Quota 並補資料面 enforcement。
- Credential lifecycle 由 transaction-aware service 擁有 lifecycle gate、mutation、audit 與 cache
  invalidation；route 不直接寫 SQL。其他 mutation 也由 control-plane service 在單一 transaction 完成。
- Provider／Model／Policy／Quota enable/disable 的原完成判準移交 W10–W12；W08 只驗 draft CRUD
  audit，避免後台顯示「已啟用」但資料面仍讀環境變數。
- Reviewer 12 點裁定：tenant、bootstrap、session、audit、identity cache、draft 狀態、CRUD CAS、
  secret ref、CORS、pool 隔離、migration 所有權均採納；五角色改三角色的建議依 W08 已核定拆解
  不採納，但已留下明確映射與延後邊界。
- 完整 diff reviewer 回報 1 high／3 medium：最後-admin 並行競態、migration timeout 汙染 pool、
  credential 多維 owner、before-state 在 transaction 外。前三項中的競態與 timeout 採納，before-state
  採納；「只能一個 owner」駁回，因 §13、`docs/identity.md` 與 W03 契約明定 credential 組成
  User／Device／Client／Agent 四個獨立維度，`num_nonnulls > 0` 是既有核定資料模型。

## 已做
- `2026-09-11T01:54+08:00` 接手既有 #16／#8 與 loop；完成分支／PR 查重，從已合 W01 的
  `origin/dev` 建 `feature/admin-api-rbac`。
- `2026-09-11T02:20+08:00` 計畫 reviewer 的 7 high／5 medium 逐點裁定；補齊 tenant、角色矩陣、
  bootstrap/session、audit transaction、identity cache、draft 狀態、CAS、secret ref、CORS、pool
  隔離與 migration 所有權。角色數依已核定 W08 保留三種，但完成五種產品角色映射。
- `2026-09-11T02:35+08:00` expand-only schema 已套進真 PostgreSQL；新增 Admin account／login
  credential／session／bootstrap claim、identity status/version、四類 draft 資源與 Admin audit 欄位。
  既有 ledger schema 測試 2/2、新 schema 測試 2/2。
- `2026-09-11T02:45+08:00` Admin auth 核心接入實際 router：bootstrap token 綁 tenant、DB one-shot；
  長效 login credential 換短效 session；DB 只存 domain-separated HMAC。並行 bootstrap、完整
  login/logout、account version 失效共 2/2 通過，0 ignored。
- `2026-09-11T03:00+08:00` 九類 tenant-scoped CRUD、UUID cursor、version CAS、draft archive、
  credential create/rotate/revoke 與同 transaction before/after audit 已接通；viewer 直打 mutation
  回 403 並 audit，operator 成功。Admin/data pool 分離、exact CORS、trusted proxy、1 MiB body limit。
- `2026-09-11T03:05+08:00` 完整 diff review 後修正：tenant advisory transaction lock 序列化
  最後-admin invariant；mutation 在同一 transaction `FOR UPDATE` 取得 before；migration 前保存並
  在成功或失敗後恢復 timeout；所有資源 SQL 改固定 statement＋參數；停用任一 Principal 維度後
  解析 fail closed，credential 所有生命週期異動同步 version／updated_at。補 `docs/admin.md`。
- `2026-09-11T03:11+08:00` PR #34 合入 `dev`（merge `729fd0e`）；Rust CI 隨後完成 success。

## 已確定不可行
- `claude -p --model opus` reviewer：組織停用 Claude subscription，無法執行；改用獨立 Codex reviewer。
- Admin credential 限成單一 User／Device／Client／Agent owner：會破壞 W03 已交付的四維 Principal
  與政策解析階層，故不採用 reviewer 此項建議。

## 證據
- `cargo test --test usage_ledger_schema`：2 passed、0 failed、0 ignored；證明新增 migration 後
  既有 ledger schema 行為仍通，證明不了舊 binary 實際啟動。
- `cargo test --test admin_schema`：2 passed、0 failed、0 ignored；跨 tenant session FK、重複
  bootstrap、未實作的 active Provider、legacy audit 寫入與 append-only 注入均被拒。
- `cargo test admin::tests`：2 passed、0 failed、0 ignored；並行 one-shot、session logout 與
  account version 變更的陰性情境皆實際變紅／拒絕。
- `cargo test admin:: --all-features`：12 passed；含最後-admin 並行競態、九類 CRUD/CAS/audit、
  viewer 403、pool 隔離、hash key overlap。漏設 `DATABASE_URL` 與誤用密碼的兩輪均在 test setup
  失敗，不列產品證據；改用 `check.sh` 的本機 DB URL 後才算有效結果。
- migration timeout 測試 1 passed：migration 前後 `statement_timeout`／`lock_timeout` 完全相同。
- Admin router 測試含 1 MiB + 1 byte body → 413；正常 operator mutation → 201。
- 陰性對照 2：把 `authorize(...).is_none()` 暫時短路後，viewer mutation 實際由 403 變 201，
  指定測試 exit 101；還原檢查後同測試通過。證明 API-layer RBAC 缺失會被測試抓到。
- `DATABASE_URL=<fresh disposable db> ./scripts/check.sh all`：84 unit + 4 schema 全綠、0 ignored；
  all-target clippy、loops、bans、deadfiles、clients、實際啟動後 secret 掃描、29/29 self-test 全綠。
  原持久本機 DB 因既有 `20260910220000` checksum mismatch 無法啟動；未修改或清空它，改用 fresh DB
  證明 migration。此環境差異移交部署段落處理，不宣稱原 DB 已修復。

## 來源
- 規格 §14（Admin 認證）、§15（RBAC）、§62（稽核）、§63（故障隔離）、§65（安全）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| Admin auth | 獨立 login credential 換短效 session | Client API Key 直接當 Admin token | 避免資料面憑證升權，符合兩套語意 |
| 刪除 | identity 停用、credential 撤銷 | 實體 DELETE | ledger 已要求歷史 FK 可追溯 |
| 稽核 | mutation＋audit 同 transaction | mutation 後 best-effort audit | 後者會留下成功變更但沒有稽核的洞 |
| 未來資源 | W08 建控制面資料契約，W10–W12 實作行為 | generic JSON 萬用表 | 後續需要 FK、索引與可檢查欄位 |
| Session | 短效 Bearer、DB hash、每次查 account role/status | Cookie session／JWT | 不需 CSRF，撤銷與降權立即生效 |
| Bootstrap | 綁 tenant 的 DB one-shot | 可重用 root token | 重啟與競態後都不能再建立第二位 initial admin |
| 未生效設定 | 只允許 `draft` | 先暴露 enabled/active | 避免控制面狀態與資料面實際行為分裂 |

## 下一步
1. W09 依 `docs/admin.md` 契約實作 Admin Web 與 read models。
