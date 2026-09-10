# LOOP: admin-web

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#8](https://github.com/DnaBoss/agent-pool-gateway/issues/8)（縮小範圍：第一版不含 SSO／核准流程）｜分支 `feature/admin-web`｜段落 `W09`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P1 |
| 前置 | `admin-api-rbac` |
| 開始 | `2026-09-11T03:17+08:00` |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
管理者打開頁面就能回答：誰在用、哪台設備、哪個 Agent、什麼模型、走哪個 Provider、
今天多少 token、花多少錢、現在誰在排隊。

## 完成判準
- [x] React ＋ TypeScript ＋ Vite ＋ TanStack Query。`lint`／`typecheck`／`test`／`build` 四關全過。
- [x] 九個頁面：Overview／Identities／Credentials／Requests／Usage／Models & Providers／
      Policies & Scheduler／System Health／Audit。
- [x] Identities 用人類階層呈現（人 → 設備 → Client → Agent），**不要迫使人記 API Key**。
- [x] Credentials 只顯示 `key_prefix` ＋ credential UUID ＋ 最後使用時間；secret hash、驗證用
      fingerprint 與完整明文永不再現。
- [x] Usage 可切八個維度（User／Device／Client／Agent／Credential／Model／Provider／Workload）
      × 四個時間範圍（今日／7 天／30 天／自訂）。
- [x] Requests 可點進單一請求看已保存的 request／attempt／trace metadata；attempt 依序呈現，
      retry／fallback 在 W17 前明示 unavailable，不假裝有完整 span 或尚不存在的歷程；
      **預設不顯示 prompt／response 內容**（因為預設不存）。
- [x] Overview 顯示 10 項明確定義的營運指標；資料尚未產生的 cost／queue 類指標回 `null` ＋
      `unavailable_reason`，不把未知顯示成 0。
- [x] Read API 全走 Admin session 與 default-deny RBAC；tenant 只取 `AdminContext`。Requests／Audit
      使用 `(timestamp,id)` cursor、limit 1..100；Usage 使用有上限 `[from,to)` 與 allowlisted dimension。
- [x] Gateway 同源提供 `/admin/` SPA，`/admin/v1` 優先；strict CSP，不載第三方 script。Session token
      只存在記憶體，401／到期／logout 清除；一次性 secret 不進 TanStack cache，modal 關閉即清除。
- [x] **陰性對照**：用 `viewer` 登入 → 寫入類按鈕不但不顯示，直接打 API 也要 403
      （UI 隱藏不算通過）。

## 已知
- 現況沒有前端目錄。
- 規格 §47：第一版明確**不追求設計感**，重點是可操作、可分析、可除錯。
- 規格 §92 列出 16 個「管理者應該一眼看得出來」的問題 → 這是驗收清單，不是願景。
- Repo 實際沒有 §48 的十項清單或 §92 的十六題原文；W09 會在 `docs/admin-web.md` 建立逐項
  data source／API field／availability／UI 驗收矩陣，但不冒充遺失的上游原文。
- `usage_records` 同一 attempt 可有 estimated 與 provider-reported；聚合只能取 provider-reported，
  沒有才 fallback estimated，並回來源與 unreconciled count。`cost_microusd` 現在皆為 NULL。
- 身分四維不是父子 FK 樹；UI 以 credential 對應的 Principal path 呈現，另列未關聯 entity，
  允許缺層與一對多，不推測 ownership。
- Team 不在現有 Principal／ledger schema，也不在 W09 八維完成判準；本輪明示 unavailable，
  不臨時虛構 team，也不宣稱已滿足完整 product MVP 的 team reporting。
- W10–W13 尚未提供 active model/policy/quota/scheduler runtime；頁面顯示真實 draft 或 unavailable，
  後續段落交付時回填，不用假資料。
- 查重：接手既有 #8 與 loop；遠端沒有 `feature/admin-web`，沒有同名開放 PR。
- 計畫 reviewer 5 high／5 medium 全採納：缺規格清單、usage 雙算/cost NULL、fingerprint 契約衝突、
  trace/retry 邊界、read route RBAC、team 缺口、identity 非樹、有界時間查詢、browser token、部署/CI。

## 已做
- `2026-09-11T03:17+08:00` W08 PR #34／merge `729fd0e`／Rust CI success 後接手；完成 branch／PR
  查重，建立 `feature/admin-web`。
- `2026-09-11T03:17+08:00` 完成獨立計畫反方審查，10 點逐項採納並收斂成上述 API、資料、
  session、部署與驗收邊界。
- `2026-09-11T03:31+08:00` 完成九個 tenant-scoped read model、actual-first usage、穩定 cursor、
  redacted audit、Vite/React/TanStack UI、同源 SPA/CSP、Docker 與 CI gate。
- `2026-09-11T03:41+08:00` 獨立 Codex diff review 回 3 P1／4 P2，全部採納：跨 session cache、
  跨期雙算、fresh tenant workflow、generated files、假 typecheck、micro-USD 與空 custom date。
- `2026-09-11T03:48+08:00` 真 gateway Playwright 走完 admin/viewer、九頁、建第一個 User 與 key、
  一次性 secret 關閉即消失、viewer UI 無寫入控制且直打 API 403；桌面/手機無 console/HTTP error。
- `2026-09-11T03:51+08:00` disposable DB 跑 `check.sh all` 全綠；production Docker image 建置並確認
  binary、SPA index、favicon 都存在。

## 已確定不可行
- 直接重建規格 §48／§92 原文：repo 沒有原清單；只能建立可驗證的本地契約並標明來源缺失。
- 顯示 credential fingerprint：W08 安全契約刻意不輸出驗證 fingerprint；改用非秘密 UUID。
- W09 顯示完整 OTLP span、W17 retry/fallback、W13 queue 狀態或 team reporting：資料源尚不存在，
  本輪只能明示 unavailable，不能用假資料宣稱完成。

## 證據
- `./scripts/check.sh all`：Rust 86 unit + 4 schema/integration、0 failed/ignored；frontend lint、
  forced project-reference typecheck、2 Vitest、production build；secret scan 與 29/29 gate selftests 全綠。
- Typecheck 陰性對照：注入 `number` 指派給 `string`，新版 gate 唯一命中 TS2322；還原後綠。
- Usage 陰性 fixture：request 在窗內、actual 晚 10 分鐘落窗外，仍只取 actual 10 而非 estimate 100。
- Playwright：1 scenario passed，1440x1000 + 375x812，九頁、fresh-tenant key flow、viewer 403；
  `document.scrollWidth <= clientWidth` 且實際檢視兩張截圖無重疊。
- `npm audit`：0 vulnerabilities。`docker build` 成功，runtime image 內三個交付產物存在。

## 來源
- 規格 §3（技術棧）、§47–§56（頁面）、§81（MVP 儀表板）、§92（最終驗收問題）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 部署 | Gateway 同源 `/admin/` 靜態 SPA | 獨立前端服務 | 不需額外 CORS、auth proxy 或部署單元 |
| Usage | attempt 級 actual-first fallback | estimated + actual 直接相加 | 避免每個 attempt token 雙算 |
| Session | 記憶體內 Bearer | localStorage／cookie | 避免持久 XSS 竊取與新增 CSRF 語意 |
| 未交付資料 | null + reason | 0／假資料 | 未知不能看起來像真的零 |

## 下一步
1. 建立 read API 契約、索引與雙 tenant／actual-first／cursor 測試。
2. 建 Vite React app、session/client/query 層與九頁真資料 UI。
3. 接同源靜態部署、CSP、frontend 四關進 `check.sh all`；Playwright desktop/mobile 驗收。
