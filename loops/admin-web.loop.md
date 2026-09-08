# LOOP: admin-web

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#8](https://github.com/DnaBoss/agent-pool-gateway/issues/8)（縮小範圍：第一版不含 SSO／核准流程）｜分支 `feature/admin-web`｜段落 `W09`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P1 |
| 前置 | `admin-api-rbac` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
管理者打開頁面就能回答：誰在用、哪台設備、哪個 Agent、什麼模型、走哪個 Provider、
今天多少 token、花多少錢、現在誰在排隊。

## 完成判準
- [ ] React ＋ TypeScript ＋ Vite ＋ TanStack Query。`lint`／`typecheck`／`test`／`build` 四關全過。
- [ ] 九個頁面：Overview／Identities／Credentials／Requests／Usage／Models & Providers／
      Policies & Scheduler／System Health／Audit。
- [ ] Identities 用人類階層呈現（人 → 設備 → Client → Agent），**不要迫使人記 API Key**。
- [ ] Credentials 只顯示遮蔽金鑰 ＋ 指紋 ＋ 最後使用時間；明文永不再現。
- [ ] Usage 可切八個維度（User／Device／Client／Agent／Credential／Model／Provider／Workload）
      × 四個時間範圍（今日／7 天／30 天／自訂）。
- [ ] Requests 可點進單一請求看完整 trace metadata（含 retry／fallback 歷程）；
      **預設不顯示 prompt／response 內容**（因為預設不存）。
- [ ] Overview 顯示規格 §48 的 10 項指標。
- [ ] **陰性對照**：用 `viewer` 登入 → 寫入類按鈕不但不顯示，直接打 API 也要 403
      （UI 隱藏不算通過）。

## 已知
- 現況沒有前端目錄。
- 規格 §47：第一版明確**不追求設計感**，重點是可操作、可分析、可除錯。
- 規格 §92 列出 16 個「管理者應該一眼看得出來」的問題 → 這是驗收清單，不是願景。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §3（技術棧）、§47–§56（頁面）、§81（MVP 儀表板）、§92（最終驗收問題）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. Overview ＋ Credentials ＋ Usage 三頁先做（MVP 驗收最吃這三頁）。
2. 再補 Requests／Identities／Health。
3. Models／Policies／Audit 最後。
