# LOOP: identity-groups

> 主體上限 **200 行**（不含「已做」）、**實時更新**。方法見全域 `start-work` skill。
> 無卡｜分支 `feature/identity-groups`｜段落 `W21`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P1 |
| 前置 | identity-credential、policy-engine、quota-ratelimit、admin-web |
| 開始 | 2026-10-08 13:45+08:00 |
| 結束 | — |
| Token | 尚無可用計量 |

## 目的

讓管理者用 email 辨識員工、用群組共用額度與權限，並能停用或恢復 Credential 而不刪除資料。

## 完成判準

- [ ] User 支援 tenant 內唯一的可選 email，後台可建立、編輯與搜尋。
- [ ] 可建立群組、加移成員，群組可綁政策與配額；user scope 優先，無命中才使用群組。
- [ ] 後台可啟用或停用 Credential，狀態變更立即使快取失效。
- [ ] 格式、靜態檢查、後端與前端測試全過。
- [ ] 陰性對照：重複 email、跨 tenant 成員、停用 Credential 與 user scope 覆蓋群組都必須被對應測試抓到。

## 已知

- 現有 users 表沒有 email/status；Credential 已有狀態機制，但後台未完整提供操作。
- 使用者於 2026-10-08 決定 email 可選但建議填寫。
- 不做群組繼承、巢狀群組、跨 tenant 群組或自動派發 Credential。

## 已做

- `2026-10-08` 完成需求與驗收規劃，尚未修改程式。
- `2026-10-20` 從最新 `dev` 建立本分支並修正 Loop 格式。

## 已確定不可行

- 無。

## 證據

- `migrations/20260910160000_identity_credentials.sql`：可核對現有 users 欄位；不證明新 schema 已實作。
- `loops/identity-credential.loop.md`：可追溯 Credential 既有能力；不證明後台操作已完成。

## 來源

- 使用者 2026-10-08 決策：email 可選但建議填寫。
- 本 repo migration、Admin API、政策與配額實作。

## Trade-off

| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| email | 可選、tenant 內唯一 | 強制填寫 | 相容既有使用者資料 |
| 群組 | 單層成員關係 | 巢狀與跨 tenant | 降低政策合併歧義 |
| Credential | 啟停保留資料 | 以刪除代替停用 | 保留稽核與恢復能力 |

## 下一步

1. S01：新增 users email/status、groups 與 group_members migration，先寫陰性測試再實作。
2. S02：擴充 policy/quota group scope 與優先序。
3. S03：完成後端 CRUD、Credential 啟停、整合測試與後台頁面。
