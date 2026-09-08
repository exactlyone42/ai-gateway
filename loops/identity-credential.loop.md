# LOOP: identity-credential

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 #TBD｜分支 `feature/identity-credential`｜段落 `W03`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P0 |
| 前置 | `workspace-foundation` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
一把金鑰對應一個人類看得懂的身分（誰／哪台設備／哪個 Client／哪個 Agent），可簽發、可撤銷、可輪替。

## 完成判準
- [ ] schema：`users` `devices` `clients` `agents` `credentials`，階層**不寫死**在型別裡。
- [ ] 完整金鑰只在建立當下回傳一次；DB 只存雜湊 ＋ `key_prefix` ＋ `fingerprint`。
- [ ] `credentials` 欄位齊全：type／display_name／secret_hash／key_prefix／fingerprint／
      user_id／device_id／client_id／agent_id／created_at／expires_at／last_used_at／revoked_at／status。
- [ ] 生命週期：create／revoke／disable／enable／rotate／expire，rotate 允許短時間 overlap。
- [ ] `PrincipalContext` 解析中介層：一次請求解析出 user／device／client／agent／credential。
- [ ] 憑證快取**有上限**；未命中且 DB 不可用時 **fail closed**（拒絕，不放行）。
- [ ] 金鑰比對走常數時間；日誌與錯誤訊息不出現金鑰明文或雜湊。
- [ ] **陰性對照 1**：`grep -r` 全表確認 DB 內沒有任何明文金鑰。
- [ ] **陰性對照 2**：撤銷一把金鑰後，同一把金鑰的下一次請求必須 401（含快取仍在的情況）。

## 已知
- 現況只有一把開機用金鑰，SHA-256 雜湊存在記憶體（`src/gateway.rs:23`），
  常數時間比對已實作（`src/gateway.rs:88`）＋ 有單元測試。這段可沿用概念，但要換成 DB 來源。
- 規格 §9 的核心設計：**Key 只是 Credential，不是 Identity**。人類看的是
  「Cash → MacBook → Claude Code → Coding Agent」，不是 `aigw_sk_f8a901...`。
- 規格 §13 建議一把金鑰對應單一 Principal，不推薦一把金鑰跨 Mac／Windows／CI／Server。
- Admin 憑證與 Client API Key 是**兩套語意**，不共用（規格 §14）。這裡只做 Client 側。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §9–§14（Identity／Credential 模型與生命週期）、§65（安全）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. schema ＋ migration。
2. 金鑰簽發／雜湊／指紋，明文只回一次。
3. Principal 解析中介層 ＋ 有上限的快取（fail closed）。
4. 生命週期六個動作 ＋ 兩個陰性對照。
