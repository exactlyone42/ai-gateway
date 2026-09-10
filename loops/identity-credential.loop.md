# LOOP: identity-credential

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#16](https://github.com/DnaBoss/agent-pool-gateway/issues/16)（生命週期部分）｜分支 `feature/identity-credential`｜段落 `W03`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P0 |
| 前置 | `workspace-foundation` |
| 開始 | 2026-09-10 15:52 CST |
| 結束 | — |
| Token | 未量得：本輪由 Codex 執行；Claude-only ledger 正確回 exit 3，未填 0 |

## 目的
一把金鑰對應一個人類看得懂的身分（誰／哪台設備／哪個 Client／哪個 Agent），可簽發、可撤銷、可輪替。

## 完成判準
- [x] schema：`users` `devices` `clients` `agents` `credentials`，階層**不寫死**在型別裡。
- [x] 完整金鑰只在建立當下回傳一次；DB 只存雜湊 ＋ `key_prefix` ＋ `fingerprint`。
- [x] `credentials` 欄位齊全：type／display_name／secret_hash／key_prefix／fingerprint／
      user_id／device_id／client_id／agent_id／created_at／expires_at／last_used_at／revoked_at／status。
- [x] 生命週期：create／revoke／disable／enable／rotate／expire，rotate 允許短時間 overlap。
- [x] `PrincipalContext` 解析中介層：一次請求解析出 user／device／client／agent／credential。
- [x] 憑證快取**有上限**；未命中且 DB 不可用時 **fail closed**（拒絕，不放行）。
- [x] 金鑰比對走常數時間；日誌與錯誤訊息不出現金鑰明文或雜湊。
- [x] **陰性對照 1**：`grep -r` 全表確認 DB 內沒有任何明文金鑰。
- [x] **陰性對照 2**：撤銷一把金鑰後，同一把金鑰的下一次請求必須 401（含快取仍在的情況）。

## 已知
- 現況只有一把開機用金鑰，SHA-256 雜湊存在記憶體（`src/state.rs:11`），
  常數時間比對已實作（`src/auth.rs:25`）＋ 有單元測試。這段可沿用概念，但要換成 DB 來源。
- 規格 §9 的核心設計：**Key 只是 Credential，不是 Identity**。人類看的是
  「Cash → MacBook → Claude Code → Coding Agent」，不是 `aigw_sk_f8a901...`。
- 規格 §13 建議一把金鑰對應單一 Principal，不推薦一把金鑰跨 Mac／Windows／CI／Server。
- Admin 憑證與 Client API Key 是**兩套語意**，不共用（規格 §14）。這裡只做 Client 側。

- W07 移交（2026-09-09）：日誌缺 `principal` 欄位（誰發的請求）。這一段做出身分解析後，在 `src/middleware.rs` 的存取紀錄補上該欄位。
- 本輪依 `cashloop` 接手；reviewer 預設先用 `claude -p --model opus`，若 Claude Code 帳號不可用，
  改用獨立的唯讀 Codex reviewer，意見由執行者依專案規範裁定。
- 第一個實作段落與完整 diff 各做一次反方審查；每段回寫已做、證據、下一步。
- 合併前必須 `./scripts/check.sh all` 全綠，並以真的壞情境證明關鍵測試會紅；完成後可自動合 `dev`。
- 只做 W03 的 schema、Client 憑證生命週期、Principal 解析與 bounded cache；Admin RBAC／audit log 留在 W08。

## 已做
- 2026-09-10 15:52 CST：接手 W03；確認卡 #16 已拆成 W03 生命週期與 W08 RBAC，指定分支在本機與遠端皆不存在，從最新 `dev` 建立 `feature/identity-credential`。
- 2026-09-10 16:12 CST：完成五表 migration、tenant 複合外鍵、CSPRNG 金鑰、HMAC-SHA256、fingerprint、六個生命週期動作、`PrincipalContext` 與 bounded cache；draft PR #27。
- 2026-09-10 16:18 CST：真 PostgreSQL 整合測試走過 create／disable／enable／rotate／expire／revoke；已進 cache 的 key 撤銷後，真 router 下一次回 401；DB miss 故障在 2 秒內回 503。
- 2026-09-10 16:45 CST：完整 diff reviewer 的 3 高／2 中全部採納；補 cache／生命週期線性化、HMAC key 版本 overlap、dev-only bootstrap、唯一 successor 與解析時到期落庫。
- 2026-09-10 16:51 CST：撤銷 cache invalidation 負面變異讓真 router 由預期 401 變 502；還原後測試恢復通過。
- 2026-09-10 17:00 CST：修正 row lock 無界阻塞；真 DB 鎖列在 1 秒內回 503，放寬成 3 秒的負面變異於 3.19 秒紅燈；最終 reviewer 回報高 0／中 0。

## 已確定不可行
- 所有環境保留 bootstrap：會繞過 tenant、DB 撤銷與生命週期；只在明確 `dev` 設定載入，其他環境誤設時拒絕啟動。
- 把 display name 寫進 `principal` 日誌：名稱可含 PII，違反安全規範；日誌只寫 credential UUID，完整可讀名稱留在 `PrincipalContext` 給控制面查詢。

## 證據
- `DATABASE_URL=... cargo test identity::tests -- --nocapture`：4 passed、0 failed、0 ignored；資料庫測試由 `sqlx::test` 建立隔離 DB 並套用真 migration。
- 明文陰性／正對照在同一資料庫測試：正常簽發後掃五表為 false；故意把 raw key 注入 `credentials.display_name` 後掃描為 true。
- `./scripts/check.sh gate`：fmt、clippy、loop、bans、deadfiles 全綠。
- reviewer：Claude Code 因組織停用 subscription 實測不可用；唯讀 Codex reviewer 指出的 tenant/cache key、輪替競態與故障語義均採納。
- 完整 diff reviewer：3 高／2 中均實證成立並採納；21 個 library 測試涵蓋 HMAC active／retiring key、單一 successor、production bootstrap 拒絕、到期落庫與 cache／revoke 鎖序。
- 負面變異：移除 `revoke` 的 cache invalidation 後，`lifecycle_is_tenant_safe_and_revocation_beats_the_cache` 單獨失敗（502 != 401）；還原後 1 passed。
- `./scripts/check.sh all`：22 passed、0 failed、0 ignored；log secret 實跑無命中，關卡自測 29/29。
- 鎖列負面變異：把 lock／statement timeout 從 1s／2s 放寬到 3s／4s，鎖列測試於 3.19s 因超過 2 秒而失敗；還原後 1 passed。
- 最終唯讀 reviewer 複查 timeout 修正後回報「已解決，未發現高／中問題」。Orca runtime 無法啟動，沒有實際派出額外 worker。
- token ledger 掃 12 份 Claude transcript，排除 synthetic 後此時窗無可量資料並回 exit 3；本輪不以 0 冒充實測值。

## 來源
- 規格 §9–§14（Identity／Credential 模型與生命週期）、§65（安全）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 金鑰儲存 | HMAC-SHA256 keyed hash ＋ SHA-256 fingerprint | 裸 SHA-256 當驗證值 | pepper 不進 DB；fingerprint 只負責候選查找，最後仍常數時間驗 HMAC |
| 身分關係 | credential 以 tenant 複合 FK 指向四個獨立維度 | User→Device→Client→Agent 固定樹 | 同一維度可重用，也不可能跨 tenant 拼成 Principal |
| 快取一致性 | 單實例 bounded LRU，生命週期操作同步失效 | TTL 到期才看見撤銷 | 第一版明定單實例；撤銷後下一個請求必須立即拒絕 |
| HMAC key 輪替 | DB 存非秘密版本 ID，設定載入 active ＋ retiring keys | 換 server key 時全體 credential 失效 | 舊版本可在短期 overlap 解析，移除過早則 503 fail closed |

## 下一步
1. 提交並推送目前 diff。
2. 等 PR #27 最新 SHA 的 CI 綠後轉 ready、合併 `dev`。
3. 回填結束時間、token ledger 結果與索引狀態。
