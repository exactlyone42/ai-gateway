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

- [x] [S01] 寫明第一個可用環境的主機、存取範圍、連接埠、網址、使用者、資料與秘密來源；重新驗證目前 `dev`、完整檢查、映像建置與空資料庫 migration，附日期與 commit。
- [x] [S02] Gateway、PostgreSQL 與 ledger spool 使用持久儲存並由受管程序啟動；程序失敗可自動重啟，重啟後 `/health`、`/ready`、Admin 與既有資料均可用。
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
- `2026-09-12` 使用者指定：部署主機用 `hawk`（`~/.ssh/config` 記為 `hawk-staging`／`18.179.17.161`／使用者 `ubuntu`），權限用 mall agent profile；CI/CD 流程參考 `ai-mall` repo。**目前這台的 22 埠從本機連不通**（連線逾時，本機對外 IP `36.233.79.181`），依規則一次失敗即停、不迴圈重試，等使用者確認白名單或跳板。
- `ai-mall` 的 CI/CD 模型（`/home/cash/orca/workspaces/ai-mall/cicd-hardening/.github/workflows`）：push `dev` 部署 QA、push `main` 部署正式；部署前保存前一版映像、migration 後驗證、部署後跑 smoke、任一階段失敗自動回到前一版；同一個 concurrency group 防止兩個部署互相覆蓋。本專案目前的 CI 只有品質檢查、沒有部署。
- 正式部署主機以外的項目——對外或 private-only 存取、網域、TLS、秘密管理來源、Provider 帳號與成本上限目前未在 repo 證實。S01 先盤點可自助完成項，真正改變範圍或需要新權限的才交使用者決定。
- 查重 `2026-09-12 12:31+08:00`：沿用 open issue #19；本機與遠端沒有 `feature/production-readiness`，GitHub 無開放 PR，同名 LOOP 原先不存在。
- 接手時工作樹有使用者提供且未提交的 `AGENTS.md`；必須保留，不得覆蓋、刪除或順手提交，除非使用者另有指示。
- `2026-09-12` 觀察待查（不在 S02 範圍，S05 再處理）：已啟用的 Provider 改連線位址回 409、停用後改成功但再啟用回 400；另外模型註冊表沒有可用模型時請求回 403 卻沒有留下帳本紀錄。三者都只在測試設定過程中遇到，尚未判斷是刻意設計還是缺陷。
- 禁止把秘密寫進 Git、命令輸出或 LOOP。遠端與 DB 多筆工作一次批次；失敗停止該外部操作，不用迴圈重試。

## 全貌與 step 索引

| step | 要交付的可用成果 | 驗收重點 | 先決 | 狀態 | 子 LOOP |
| --- | --- | --- | --- | --- | --- |
| S01 | 可執行的環境基線與差距清單 | 目標環境、設定來源、最新程式與 DB 相容性有現況證據 | W00-W19 | 已完成 `2026-09-12` | 不需要，見下表 |
| S02 | 重啟後仍可用的單實例服務 | 受管啟動、持久 DB／spool、健康與就緒判斷 | S01 | 已完成 `2026-09-12` | 不需要 |
| S03 | 可輪替且不外洩的正式秘密 | 無 placeholder、一次 bootstrap、輪替與撤銷可驗 | S01、S02 | 待開始 | 進入前展開 |
| S04 | 兩種 Client 的真實完整流程 | Claude Code、Codex、串流、帳務與 Admin 可對回 | S03 | 待開始 | 進入前展開 |
| S05 | 故障、備份、還原與回退 | 可丟棄環境注入失敗，資料與服務依契約恢復 | S02-S04 | 待開始 | 進入前展開 |
| S06 | 看得到問題且知道怎麼處理 | 真告警、門檻、負責人、事故與成本控制 | S04、S05 | 待開始 | 進入前展開 |
| S07 | 經授權的正式推出與驗收 | CI、人工關卡、部署、smoke、監看、回退 | S01-S06 | 待開始 | 進入前展開 |

每個整體驗收項已標示負責 step。只細拆當前 S01；進入後續 step 前補齊該部分的成功、失敗、銜接與實際操作驗收。若子檔增加，主檔只保留摘要與連結。

## S01 環境基線（2026-09-12 實測，commit `7c17a80`）

重跑方式：`PROVIDER_ALLOWED_TENANT_IDS=<uuid> docker compose -f deploy/docker-compose.yml up --build`，再查 `/health`、`/ready` 與 `_sqlx_migrations`。

| 項目 | 現況 | 缺口／需使用者決定 |
| --- | --- | --- |
| 主機 | 只有本機開發機驗過整套可跑 | 使用者指定 `hawk`；目前從本機 22 埠連不通，待確認白名單或跳板 |
| 存取範圍 | 只綁 `127.0.0.1`，未對外 | 對外或僅內網、要不要反向代理與 TLS |
| 連接埠 | 閘道 8080、資料庫 15434（本次基線）／15432（既有開發） | 正式連接埠未定 |
| 網址 | 無網域、無 TLS | 網域與憑證來源 |
| 使用者 | 尚未建立任何 Admin；compose 預設關閉開發用 bootstrap | S03 做一次性 Admin 建立 |
| 持久資料 | 具名磁碟區 `postgres-data`、`ledger-spool`；重啟後資料仍在 | 備份與還原未做（S05） |
| 秘密來源 | 全部是預設值：Provider 金鑰 `replace-me`、憑證雜湊金鑰是開發字串 | 真金鑰來源、Provider 帳號與花費上限 |
| Client | 連法寫在 `docs/client-compatibility.md`，尚未實測 | S04 各跑一個有上限的真請求 |

## 當前輪次

- 輪次：1
- 當前 step：S01
- 本輪成果：形成一份以目前主機與最新 `dev` 為準的環境基線，明確分出「已存在、缺少、需人決定」。
- 完成條件：記錄 commit、工作樹與 owner；跑完整檢查與 production image build；用全新可丟棄 DB 驗 migration；盤點部署入口、持久資料、連接埠、秘密來源、Client 與成本上限，不輸出秘密。
- 最可能推翻方案的假設：目前 Docker／服務設定能在不碰其他專案與共享 DB 的情況下形成持久單實例；先以本機設定與一次有界啟動驗證。
- 目前動作：分支 `feature/production-readiness` 與 draft PR #54 已建立。完整檢查已綠。接著依審查意見，用另一組具名磁碟區把 `deploy/docker-compose.yml` 整套拉起來，一次同時證明映像建得出來、11 個資料庫升級步驟從空庫套得完、`/ready` 會綠、重啟後資料還在。
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
| 2026-09-12 12:51+08:00 | 開工審查指出：空庫套 migration＋單獨建映像都碰不到 compose 整套能否跑起來；只套到 3/11 的舊開發資料庫不該當基線 | 1→1（不升版，只改 S01 做法） | S01 | S01 改用「另一組具名磁碟區把整套拉起來」當單一證據；放棄研究舊 15432 資料庫如何升級，改為丟棄重建 |

## 已做

- `2026-09-12 13:34+08:00` **S02 完成，過程中抓到一個「健康燈是綠的、帳本其實壞掉」的嚴重缺陷**。
  缺陷：容器以非 root 使用者執行，但 Docker 具名磁碟區的掛載點屬 root，閘道**寫不進帳本暫存目錄**。啟動照樣成功、健康檢查照樣綠燈——用量紀錄的耐久保護等於整個失效，而且沒有任何徵兆。
  兩處修正：① 映像事先建立該目錄並設好擁有者，空磁碟區會沿用；② 啟動時實際寫一次探測檔，寫不進去就拒絕啟動（fail closed，符合既有「稽核耐久性失敗就擋」原則）。
  陰性對照（同一個映像）：把磁碟區擁有權改回 root → 啟動被擋，訊息三層可讀「無法初始化用量帳本 → 帳本暫存不可用 → 權限不足」，並持續重啟；改回正確擁有者 → 啟動成功、目錄可寫。單元測試也補上「不可寫目錄必須開不起來」，實測拿掉探測後該測試 FAILED。
  S02 其餘判準證據：具名磁碟區保存資料庫與帳本暫存；程序自己結束時 Docker 自動重啟（觀察到連續 7 與 10 次）；硬砍後重啟 `/ready` 200、管理員帳號與登入權杖仍可用。
  另外實測資料面已可端到端運作：用後台建立使用者與金鑰、註冊 Provider 與模型後，一個請求走完身分、政策、模型選擇並呼叫上游（刻意指向連不上的位址）→ 502，帳本留下一筆 `failed`。資料庫停掉時 `/ready` 回 503、`/health` 回 200，請求以 503 擋在花錢之前。
- `2026-09-12 13:18+08:00` 依 README 現在寫的方式實測整條「起得來、進得去、活得過重啟」：
  - 全新空資料庫、用 admin bootstrap 那組設定啟動 → 兩個容器 healthy。
  - `POST /admin/v1/bootstrap` 建立第一位管理員回 **201**；同一組再打一次回 **409**，資料庫裡 `admin_accounts` 1 筆、`admin_bootstrap_claims` 1 筆、稽核紀錄 `admin.bootstrap` 1 筆——一次性限制真的擋得住。
  - 用回傳的登入權杖建立 session 回 **201**；亂填權杖回 **401**。
  - 對閘道容器送 SIGKILL 再啟動：`/ready` 回 200、管理員帳號還在、同一組登入權杖仍可建立 session。資料活得過硬砍。
  兩次無效嘗試照實記下：① 在容器內 `kill -9 1` 不會生效（PID 1 收不到預設處置的訊號），② `docker kill` 之後 Docker 視為人工停止，不會套用自動重啟。**自動重啟的真實證據是稍早那次啟動失敗迴圈**：程序自己結束，Docker 連續重啟 10 次（`restarts=10`、`status=restarting`）。
- `2026-09-12 13:13+08:00` 完工審查（反方，claude -p opus）提出三個真問題，全部修掉：
  1. 新增的「沒有任何可用租戶就擋下」沒有測試，刪掉那行照樣全綠。已把組租戶清單抽成純函式並補測試；實測刪掉檢查後該測試 FAILED。
  2. 上一版的 spool 清理等待放在中止服務之後，而中止不是當場結束，計數可能還是 0、等待立刻返回，競態只縮小沒消除。改成等待「目錄只剩一個 ready 檔」這個看得到的結果，最多等 5 秒，等不到就讓測試紅，不再靠時序運氣。
  3. README 的一鍵啟動會生出「起得來但沒人能用」的站台。已改成用 admin bootstrap 那組設定（同時滿足租戶要求），並寫明啟動後還要先建立第一位管理員、Provider 金鑰仍是預設值。
  審查另一項「那個租戶 uuid 指向不存在的租戶」查證後不成立：資料庫沒有 tenants 表，tenant 只是欄位值。
  完整檢查在修完後 EXIT=0：228 passed／0 failed／0 ignored。
- `2026-09-12 13:04+08:00` **照文件的一鍵啟動其實起不來，已修好並實測**。依審查意見改用「另一組具名磁碟區把 `deploy/docker-compose.yml` 整套拉起來」當單一證據，連續撞到三個缺陷：
  1. `ADMIN_BOOTSTRAP_TENANT_ID` 未設定時，compose 仍送出空字串，設定解析把空字串拿去解 UUID 就失敗，容器無限重啟。同組的 `ADMIN_BOOTSTRAP_TOKEN` 早就把空字串當未設定，兩者不對稱。已改成一致（commit `7fad281`），並補上「拿掉修正就會變紅」的測試（實測還原舊行為後 FAILED）。
  2. compose 從未轉送 `PROVIDER_ALLOWED_TENANT_IDS`，而 compose 預設關閉開發用 bootstrap，於是沒有任何租戶可用 Provider 憑證，啟動被擋——這個「擋下來」是對的（fail closed），錯的是沒有可設定的入口。已在 compose 補上該變數。
  3. 啟動失敗只印出變數名稱（`Error: Registry(InvalidProviderProfile)`、`Error: NoAllowedTenant`），維運看不出要改什麼。`main` 改為印出可讀訊息與整串成因；設定層也補上直接點名要設哪個變數的錯誤。
  實測（同一個映像、同一組設定）：不設租戶 → `agent-pool-gateway stopped with an error: no tenant may use the providers: set PROVIDER_ALLOWED_TENANT_IDS, or ADMIN_BOOTSTRAP_TENANT_ID with ADMIN_BOOTSTRAP_TOKEN`；設了租戶 → 兩個容器都 healthy、`/health` 200、`/ready` 200、`/admin/` 200。空資料庫從零套用 **11/11** 個升級步驟成功（`_sqlx_migrations` 11 筆全部 success，public schema 22 張表）。整套 restart 後 migration 指紋不變（`750a74d7…`）、`/ready` 回 200，證明資料在具名磁碟區上活得過重啟。連接埠只綁 `127.0.0.1`（8080 與 15434），沒有暴露到區域網路。
- `2026-09-12 12:51+08:00` S01 進行中：完整檢查第一次跑出**紅燈**（EXIT=101）——`routes::tests::dropping_an_openai_client_closes_the_provider_stream` 在 `src/routes.rs` 清理 spool 目錄時 panic（`DirectoryNotEmpty`）。單獨跑會過，併發跑才紅。直接量測找出原因：中止服務任務後 `pending_finalizations()=1`，等待前目錄是 `.next`／`.reserved`／`.working` 三個保留檔，等待後只剩一個 `.ready`——背景改名與 `remove_dir_all` 互相搶。兩個同型測試改成先 `wait_for_finalizations()` 再清理（commit `355afd9`）。重跑 `./scripts/check.sh all` EXIT=0：226 passed／0 failed／0 ignored，整合測試 3＋2 通過，gate 自我測試 29/29。
- `2026-09-12 12:42+08:00` Claude Code 接手：重查 owner 與重複工作——issue #19 仍 OPEN（assignee DnaBoss，label `status:backlog`）、GitHub open PR 為 0、遠端只有 `dev`／`main`／`feat/T-06-virtual-key-store`，沒有 `feature/production-readiness`。確認無重複後從 `dev`（`7ae94af`）建立 `feature/production-readiness`。工作樹的未追蹤 `AGENTS.md` 保持原狀，未加入提交。
- `2026-09-12 12:31+08:00` Codex 依新版 cashloop 建立 W20 主 LOOP；查得 open issue #19 可沿用，沒有同名 LOOP、分支或開放 PR。只建立交接計畫，尚未實作或部署。

## 已確定不可行

- 把已停止的臨時 `18080` Gateway 與可丟棄 DB 當正式環境：重啟後不存在，也沒有真 Provider 憑證。
- 為了沿用既有 `15432` DB 而修改 migration checksum 或 migration 歷史：會破壞共享資料可信度；改用正確環境與可驗證升級路徑。`2026-09-12` 實查：該資料庫只套用 11 個 migration 中的前 3 個，不具代表性，基線改用全新資料庫。
- 用 placeholder Provider key、把秘密寫進 `.env` 後提交，或在 LOOP 留明文：無法安全交付。
- 尚有單實例鎖與單機 spool 時直接宣稱 HA：證據不支持；先完成可用單實例，HA 依量測與共享耐久設計另行規劃。
- 只看 `/health`、HTTP 200、截圖或 build 成功就宣布可用：必須走 Client → Gateway → Provider → ledger → Admin 的真實流程。

## 證據

- `loops/README.md`：W00-W19 均標示已交付；證明程式段落有交付紀錄，不能證明現在有運行中的正式服務。
- `docs/operations.md`：定義環境隔離、分階段推出、migration、spool、關機與告警；證明已有操作契約，不能證明目標環境已照做。
- `AGENTS.md`／`CLAUDE.md`：`dev` 是整合分支、`main` 是正式；證明部署關卡，不代表本 LOOP 自動取得合併或部署授權。
- `2026-09-12` 查重：GitHub open issue #19；open PR 為 0；同名本機／遠端分支為 0。這只證明當時未發現重複工作，接手仍須再查 owner 與最新遠端狀態。
- `2026-09-12` 完整檢查兩次紀錄：第一次 EXIT=101（1 個測試紅）、修正後 EXIT=0（226 passed／0 failed／0 ignored／0 skipped）。這證明目前 `dev` 加上該修正可通過全部既有 gate，不證明服務在任何主機上跑得起來。
- `2026-09-12` spool 競態的直接量測（`pending_finalizations()=1`、三個保留檔改名成一個 `.ready`）。這證明失敗原因是測試清理與背景改名搶同一個目錄，不證明正式路徑上的 ledger 有問題——正式路徑不會在改名中途刪掉 spool 目錄。
- `2026-09-12` 帳本暫存目錄不可寫的雙向對照：擁有權壞掉 → 啟動被擋且訊息三層可讀；擁有權正確 → 啟動成功且可寫。這證明「壞掉會被發現」，不證明磁碟滿或磁碟區遺失時的行為（S05 才驗）。
- `2026-09-12` 第一位管理員流程：201 建立、409 擋第二次、201 登入、401 擋亂填權杖。這證明控制面入口可用且一次性限制有效，不證明資料面能打到真 Provider。
- `2026-09-12` 一鍵啟動的雙向對照：不設租戶時啟動被擋且訊息點名要設哪個變數；設了租戶則整套 healthy。這證明「擋得住」與「起得來」兩邊都成立，不證明真 Provider 請求可用——目前金鑰仍是 `replace-me`。
- `2026-09-12` 空資料庫 11/11 migration 成功、重啟後指紋不變。這證明資料結構升級與單機持久化可行，不證明備份、還原或跨主機搬遷（S05 才驗）。
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

1. 等使用者確認 `hawk` 的連線方式（白名單或跳板）。通了之後先只做唯讀盤點：作業系統、Docker、記憶體、磁碟、已佔用連接埠、現有服務，寫進 S01 基線表。
2. 不等連線也能做的：依 `ai-mall` 模型補本專案的部署流程草稿（push `dev` 進 staging、push `main` 進正式；部署前留前一版、部署後 smoke、失敗自動回退）。未經實機驗證前不合併、不啟用。
3. S03 秘密輪替：憑證雜湊金鑰輪替、Provider 金鑰改由核准來源注入、確認 Git 與日誌無外洩。
4. S05 再處理本輪記在「已知」的三個待查項。
