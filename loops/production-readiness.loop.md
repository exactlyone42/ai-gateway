# LOOP: production-readiness

> 主 LOOP；主體上限 200 行（不含「已做」）、實時更新。
> 卡 [#19](https://github.com/DnaBoss/agent-pool-gateway/issues/19)｜預定分支 `feature/production-readiness`｜段落 `W20`｜計畫版本 `1`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中（S01） |
| 優先度 | P0 |
| 前置 | W00-W19 已交付 |
| 開始 | 2026-09-12 12:42+08:00（Claude Code 接手；Codex 2026-09-12 12:31 建檔） |
| 結束 | 待完成 |
| Token | 來源 `./scripts/token-ledger.sh --slug production-readiness`（讀 Claude Code transcript）；基準＝本 LOOP 開始欄時間，結束時讀同一來源 |

## 目的

讓核准的使用者能從 Claude Code 與 Codex 穩定使用 Agent Pool Gateway；服務重啟後設定、身分與帳務仍在，故障可查、可回復、可回退，不把一次本機預覽冒充正式可用。

## 完成判準

- [ ] [S01] 寫明第一個可用環境的主機、存取範圍、連接埠、網址、使用者、資料與秘密來源；重新驗證目前 `dev`、完整檢查、映像建置與空資料庫 migration，附日期與 commit。
- [ ] [S02] Gateway、PostgreSQL 與 ledger spool 使用持久儲存並由受管程序啟動；程序失敗可自動重啟，重啟後 `/health`、`/ready`、Admin 與既有資料均可用。
- [ ] [S03] 正式環境沒有 placeholder；Provider、credential HMAC、Admin bootstrap 等秘密只由核准來源注入。完成一次性 Admin 建立與憑證輪替驗證，Git、log、LOOP 與 API 回應均無秘密外洩。
- [ ] [S04] Claude Code 與 Codex 各以一個有上限的真實請求通過 Gateway；串流、取消、錯誤、身分、用量與 Admin 查詢串得起來，帳本數字可對回 Provider 回報。
- [ ] [S05] 在 staging 或可丟棄環境完成負面驗證：Gateway 重啟、DB 暫停、Provider 拒絕／逾時、spool 恢復、備份還原及版本回退；不得用正式流量做破壞性注入。
- [ ] [S06] 建立能實際收到的告警與事故手冊，至少涵蓋服務不可用、DB／spool、Provider 認證、用量異常、5xx、延遲與金鑰輪替；每項有負責人、門檻、處理與解除方式。
- [ ] [S07] `./scripts/check.sh all`、CI、部署前清單與整體操作流程全綠；取得既有規則要求的正式部署授權後才合 `main`／部署，完成部署後 smoke、監看與回退證據。
- [ ] [整體] 從 Client 發出請求到 Admin 查得身分、模型、用量與錯誤的完整流程實際操作成功；成功、失敗、略過及未驗證分開回報。
- [ ] [陰性對照] 拿掉一項正式必要條件（例如改回 placeholder Provider key、使用非持久 spool 或讓 `/ready` 在 DB 中斷時仍回 200），對應檢查必須變紅；還原後再通過。

## 已知

- `loops/README.md` 記錄 W00-W19 已交付；這只證明程式段落完成，不證明目前有持續運行的正式服務。
- `2026-09-11` 曾用可丟棄 PostgreSQL 與 `127.0.0.1:18080` 實測 Admin Web 登入；服務與臨時 DB 後來已停止，不能當正式部署證據。
- 當時 `8080`／`5173` 被另一專案占用，且既有 `15432` DB 發生 migration checksum 不一致；這些是舊證據，S01 必須重新查一次，不能直接沿用，也不能修改共享 DB 來硬過。
- `docs/operations.md` 已定義 migration、spool、關機與分階段推出；`main` 代表正式，合併與部署仍受 `AGENTS.md`／`CLAUDE.md` 的人工與 CI 關卡限制。
- 現有架構第一版是單實例，ledger spool 是單機恢復機制。沒有量測與共享耐久設計前不得宣稱 HA（多台同時服務仍不中斷）；需要 HA 時另做有證據的 replan 或新 LOOP。
- 正式部署主機、對外或 private-only 存取、網域、TLS、秘密管理來源、Provider 帳號與成本上限目前未在 repo 證實。S01 先盤點可自助完成項，真正改變範圍或需要新權限的才交使用者決定。
- 查重 `2026-09-12 12:31+08:00`：沿用 open issue #19；本機與遠端沒有 `feature/production-readiness`，GitHub 無開放 PR，同名 LOOP 原先不存在。
- 接手時工作樹有使用者提供且未提交的 `AGENTS.md`；必須保留，不得覆蓋、刪除或順手提交，除非使用者另有指示。
- 禁止把秘密寫進 Git、命令輸出或 LOOP。遠端與 DB 多筆工作一次批次；失敗停止該外部操作，不用迴圈重試。

## 全貌與 step 索引

| step | 要交付的可用成果 | 驗收重點 | 先決 | 狀態 | 子 LOOP |
| --- | --- | --- | --- | --- | --- |
| S01 | 可執行的環境基線與差距清單 | 目標環境、設定來源、最新程式與 DB 相容性有現況證據 | W00-W19 | 待開始 | 視複雜度建立 |
| S02 | 重啟後仍可用的單實例服務 | 受管啟動、持久 DB／spool、健康與就緒判斷 | S01 | 待開始 | 進入前展開 |
| S03 | 可輪替且不外洩的正式秘密 | 無 placeholder、一次 bootstrap、輪替與撤銷可驗 | S01、S02 | 待開始 | 進入前展開 |
| S04 | 兩種 Client 的真實完整流程 | Claude Code、Codex、串流、帳務與 Admin 可對回 | S03 | 待開始 | 進入前展開 |
| S05 | 故障、備份、還原與回退 | 可丟棄環境注入失敗，資料與服務依契約恢復 | S02-S04 | 待開始 | 進入前展開 |
| S06 | 看得到問題且知道怎麼處理 | 真告警、門檻、負責人、事故與成本控制 | S04、S05 | 待開始 | 進入前展開 |
| S07 | 經授權的正式推出與驗收 | CI、人工關卡、部署、smoke、監看、回退 | S01-S06 | 待開始 | 進入前展開 |

每個整體驗收項已標示負責 step。只細拆當前 S01；進入後續 step 前補齊該部分的成功、失敗、銜接與實際操作驗收。若子檔增加，主檔只保留摘要與連結。

## 當前輪次

- 輪次：1
- 當前 step：S01
- 本輪成果：形成一份以目前主機與最新 `dev` 為準的環境基線，明確分出「已存在、缺少、需人決定」。
- 完成條件：記錄 commit、工作樹與 owner；跑完整檢查與 production image build；用全新可丟棄 DB 驗 migration；盤點部署入口、持久資料、連接埠、秘密來源、Client 與成本上限，不輸出秘密。
- 最可能推翻方案的假設：目前 Docker／服務設定能在不碰其他專案與共享 DB 的情況下形成持久單實例；先以本機設定與一次有界啟動驗證。
- 目前動作：讀最新專案規則與本 LOOP，確認 owner 後建立既定分支、推遠端並開 draft PR，再執行 S01。
- 阻塞與解除條件：尚無；正式網址、Provider 帳號或部署權限若確實不可取得，只阻塞對應 step，不阻塞基線與可審查設定。
- 後續順序：S01 → S02 → S03 → S04 → S05 → S06 → S07。

## 檢查點與重新規劃

- 開始、每輪結束、每個 step 結束、使用者更正、重大失敗、新證據及每隔 300 秒都檢查一次；五分鐘檢查只看停滯、偏離與新阻塞，不每次重跑完整測試。
- 每次記：時間、真實版本與證據、本輪／step 驗收、是否偏離整體成果、對前後依賴的影響、接續／重排／等待／完成及理由、下次檢查時間。
- 每個檢查點都可按證據 replan；沒有實質變化就不改計畫，但仍留一句依據。連續兩輪沒有新證據或可驗收進展，必須換驗證方法、縮小問題或提出真正需要的決策。
- replan 回寫：觸發證據 → 舊目標 → 保留／取消／延後 → 受影響 step → 新順序與驗收 → 增加的工作／時間 → 下一步。
- 跨 step 改變要升計畫版本並同步主檔與受影響子檔；只影響本輪就更新該 step。保留舊編號與去向，不全面重寫無關部分。
- 新證據推翻已完成項目時，重新打開該勾選與依賴它的後續 step；不得降低驗收、猜測 API、用換計畫掩飾沒成果或擴張未授權範圍。

| 時間 | 觸發與證據 | 舊版→新版 | 受影響 step | 決定 |
| --- | --- | --- | --- | --- |
| 2026-09-12 12:31+08:00 | 建立主 LOOP；目前只有一次性本機預覽，沒有持續服務證據 | 無→1 | S01-S07 | 先做 S01，不宣稱已部署 |

## 已做

- `2026-09-12 12:42+08:00` Claude Code 接手：重查 owner 與重複工作——issue #19 仍 OPEN（assignee DnaBoss，label `status:backlog`）、GitHub open PR 為 0、遠端只有 `dev`／`main`／`feat/T-06-virtual-key-store`，沒有 `feature/production-readiness`。確認無重複後從 `dev`（`7ae94af`）建立 `feature/production-readiness`。工作樹的未追蹤 `AGENTS.md` 保持原狀，未加入提交。
- `2026-09-12 12:31+08:00` Codex 依新版 cashloop 建立 W20 主 LOOP；查得 open issue #19 可沿用，沒有同名 LOOP、分支或開放 PR。只建立交接計畫，尚未實作或部署。

## 已確定不可行

- 把已停止的臨時 `18080` Gateway 與可丟棄 DB 當正式環境：重啟後不存在，也沒有真 Provider 憑證。
- 為了沿用既有 `15432` DB 而修改 migration checksum 或 migration 歷史：會破壞共享資料可信度；改用正確環境與可驗證升級路徑。
- 用 placeholder Provider key、把秘密寫進 `.env` 後提交，或在 LOOP 留明文：無法安全交付。
- 尚有單實例鎖與單機 spool 時直接宣稱 HA：證據不支持；先完成可用單實例，HA 依量測與共享耐久設計另行規劃。
- 只看 `/health`、HTTP 200、截圖或 build 成功就宣布可用：必須走 Client → Gateway → Provider → ledger → Admin 的真實流程。

## 證據

- `loops/README.md`：W00-W19 均標示已交付；證明程式段落有交付紀錄，不能證明現在有運行中的正式服務。
- `docs/operations.md`：定義環境隔離、分階段推出、migration、spool、關機與告警；證明已有操作契約，不能證明目標環境已照做。
- `AGENTS.md`／`CLAUDE.md`：`dev` 是整合分支、`main` 是正式；證明部署關卡，不代表本 LOOP 自動取得合併或部署授權。
- `2026-09-12` 查重：GitHub open issue #19；open PR 為 0；同名本機／遠端分支為 0。這只證明當時未發現重複工作，接手仍須再查 owner 與最新遠端狀態。
- `2026-09-11` Admin 預覽曾回 200 且 login session 建立成功，之後 `18080` 已無服務。這證明 UI 流程曾可操作，不能證明持久部署或真模型請求。

## 來源

- `docs/operations.md`：部署、migration、spool、關機、告警與失敗策略。
- `docs/architecture.md`：單實例、多實例、資料面與控制面故障邊界。
- `docs/security-and-compliance.md`：秘密、Provider 與訂閱席次界線。
- `docs/client-compatibility.md`：Claude Code 與 Codex 的自訂端點及已知限制。
- `docs/admin.md`、`docs/admin-web.md`：Admin bootstrap、session、權限與實際操作流程。
- `docs/engineering-standards.md`：測試、負面對照、Git、正式分支與資料庫規則。
- GitHub issue [#19](https://github.com/DnaBoss/agent-pool-gateway/issues/19)：HA 部署、服務水準與事故控制的既有工作入口。

## Trade-off

| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 第一個可用版本 | 先證明持久單實例，再依數字規劃 HA | 未量測就上多實例 | 現有單實例鎖與本機 spool 不支持空口宣稱 HA |
| 計畫結構 | 一份主 LOOP 列全貌，後續 step 按需要建子檔 | 一開始建立七份空子檔 | 讓 Claude 先看全貌，又不讓遠期細節快速過時 |
| 秘密 | 由核准來源在執行時注入 | Git 內 `.env` 或硬編碼 | 降低外洩與輪替風險 |
| 驗收 | 少量、有成本上限的真請求＋失敗注入 | 只用 mock 或無上限壓測 | 真流程可證明可用，同時控制成本與風險 |
| 正式推出 | staging／pilot／canary 後擴大 | 一次全量切換 | 問題可提早發現並回退 |

## 下一步

1. Claude Code 執行 `/cashloop loops/production-readiness.loop.md`，先重新查 owner、issue #19、分支與 PR；有新 owner 就接手，不另開工作。
2. 無重複工作才從最新 `dev` 建立並推送 `feature/production-readiness`，開 base `dev` 的 draft PR；不把未授權部署當成已允許。
3. 執行 S01，先回寫開始時間、token 基準、最新證據與第一次方向檢查；完成 S01 檢查點後才展開 S02。
