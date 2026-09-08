# LOOP: admin-api-rbac

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 #TBD｜分支 `feature/admin-api-rbac`｜段落 `W08`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P1 |
| 前置 | `identity-credential` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
後台的權限在 API 層擋得住，不是靠前端藏按鈕；每個控制面操作都留得下「誰做了什麼、改前改後」。

## 完成判準
- [ ] Admin 認證與 Client API Key **兩套語意分開**：Admin 走短效 access token／安全 session，
      Client 走 API Key。兩者不共用驗證路徑。
- [ ] 三角色 `admin`／`operator`／`viewer`，權限邊界照規格 §15。
- [ ] RBAC 在 **API layer** 強制，不是 UI 層。
- [ ] 控制面 CRUD：User／Device／Client／Agent／Credential／Provider／Model／Policy／Quota。
- [ ] audit log 欄位齊全：actor／action／target／before／after／timestamp／source IP／request_id。
      至少覆蓋：憑證 create/revoke/rotate、模型啟停、Provider 啟停、政策更新、配額更新、RBAC 變更。
- [ ] Admin API 與推論 API 的權限完全分離；CORS 從嚴。
- [ ] Admin 掛掉不得影響進行中的推論（規格 §63）。
- [ ] **陰性對照 1**：`viewer` 憑證直接打 `admin` 端點（繞過 UI）→ 必須 403 並留 audit。
- [ ] **陰性對照 2**：把 RBAC 檢查從某個端點移除 → 對應測試必須變紅。

## 已知
- 現況沒有任何 Admin API，也沒有角色概念。
- 規格 §14／§15 明確：不共用 credential semantics；RBAC 必須 API 層強制。
- 第一版**不做** SSO 與四眼核准流程（既有卡 #8 的範圍縮小，見 `docs/work-breakdown.md`）。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §14（Admin 認證）、§15（RBAC）、§62（稽核）、§63（故障隔離）、§65（安全）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. Admin 認證（短效 token）＋ 角色模型。
2. RBAC middleware ＋ 兩個陰性對照。
3. 控制面 CRUD ＋ audit 寫入（before／after 快照）。
