# LOOP: policy-engine

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#17](https://github.com/DnaBoss/agent-pool-gateway/issues/17)｜分支 `feature/policy-engine`｜段落 `W11`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 已交付 |
| 優先度 | P1 |
| 前置 | `identity-credential`, `model-registry-router` |
| 開始 | `2026-09-11T05:11:00+08:00` |
| 結束 | `2026-09-11T05:56:48+08:00`（46 分） |
| Token | 主執行為 Codex，現有 Claude transcript ledger 無法取得精確值；獨立 reviewer `274,428` |

## 目的
同一把金鑰的請求，效力政策是可預測、可解釋、可稽核的——查得到「為什麼這次只給 standard」。

## 完成判準
- [x] 政策可掛在六個層級：Global／User／Device／Client／Agent／Credential，再加 Request hint。
- [x] 合併優先序明確且有單元測試覆蓋每一層的覆寫行為。
- [x] **Request hint 不得突破上層 hard constraint**。
- [x] 四個維度**互相獨立**，不得壓成單一 `priority = 1~10`：
      workload_class（critical／interactive／foreground／automation／background／batch）、
      urgency（immediate／normal／low／idle）、
      quality（economy／standard／advanced／premium）、
      budget（cheap／normal／unrestricted）。
- [x] 支援 `deadline_at` 或 `max_queue_time`。
- [x] 來源預設政策：例如 Claude Code = interactive／immediate／premium；
      VS Code 補全 = interactive／immediate／economy；研究 Agent = background／low／premium；
      記憶 Agent = batch／idle／economy。
- [x] `EffectivePolicy` 寫進 `gateway_requests`，事後查得到當時的決策依據。
- [x] **陰性對照 1**：Agent 上限 `standard`、請求 hint 要 `premium` → 實際必須是 `standard`。
- [x] **陰性對照 2**：把某一層的 hard constraint 檢查移除 → 對應測試必須變紅。

## 已知
- 現況沒有政策概念，只有一份環境變數模型白名單。
- W08 擁有 Policy 的 tenant-safe、immutable-version draft CRUD schema；W11 接手啟用與合併行為，
  不能另建平行資料表，啟用／換版必須留下 audit。
- 規格 §23 的理由很關鍵：`autocomplete` 是「非常急 ＋ economy」，
  `architecture analysis` 是「不急 ＋ premium」。壓成單一優先權數字就表達不出來。
- 規格 §26：合併結果要存進請求記錄，否則事後無法解釋。
- 查重：沿用既有卡 #17 與本 loop；開工時沒有同名 branch 或 open PR，因此建立
  `feature/policy-engine`，不開第二張卡。
- 卡面的 company 對應 tenant-scoped Global；repo 規格與現有 identity schema 沒有 team entity，
  W11 不新增平行 team ownership。若日後引入 team，須以新 identity migration 擴充 policy scope。
- Reviewer 先嘗試 `claude -p --model opus`；若既有訂閱限制重現，改用獨立 `codex exec`。
  計畫定案與完整 diff 各審一次；高／中嚴重度須修正或以規格與實測裁定。
- 每段守門：相關 Rust/DB 測試與陰性對照；提交前 `./scripts/check.sh all`。外部依賴先自行
  打通；只做安全性、必要依賴與 W11 目標；完成後依既有授權合入 `dev`。

## 已做
- `2026-09-11T05:11+08:00` W10 merge `86395e3`、收工 commit `2251a62` 後接手；確認卡 #17、
  loop、分支與 PR ownership，無重複實作，建立 `feature/policy-engine`。
- `2026-09-11T05:27+08:00` 完成 typed policy、tenant-scoped 七層合併、request hint、hard ceiling、
  model allowlist／deny、deadline、來源預設範例，以及不可變 decision snapshot 落帳。
- `2026-09-11T05:27+08:00` 完成 policy draft schema 驗證、同 scope revision 原子替換、CAS、audit，
  以及 Admin Web 建立／編輯／啟停／封存與 request decision 檢視。
- `2026-09-11T05:28+08:00` full gate 與真實 gateway Playwright 綠；反方 diff review 待執行。
- `2026-09-11T05:43+08:00` 獨立 Codex diff review 完成（274,428 tokens）：3 high、8 medium
  全數採納。修正 credential 輪替繼承、provider 前 deadline、舊資料 migration、拒絕 snapshot、
  replacement audit、queue precedence、bounded cache、可辨識 target、併發啟用、route/service 分層與測試。
- `2026-09-11T05:49+08:00` 真實舊版 DB 升級與新 gateway Playwright 重驗通過；另修正平行測試中
  tracing subscriber 跨執行緒遺失造成的偶發失敗。
- `2026-09-11T05:56+08:00` 功能 commit `912bc79` 經 PR #38 的 Rust／sync CI 全綠，合入
  `dev` merge `e7bcc4c`；完整守門 exit 0，W11 交付。

## 已確定不可行
- `claude -p --model opus` 被組織層停用訂閱存取，單次失敗後改用獨立 `codex exec`，不重試。
- 共用 `gateway` DB 有既有 migration checksum 漂移（`VersionMismatch(20260910220000)`），不能當本輪
  啟動證據；改用全新 `apg_w11_verify`／`apg_w11_ui2`，不清空或改寫共用資料。
- 由 User-Agent 字串硬編碼來源預設不穩定；來源預設改成可配置 Client／Agent scope policy。
- direct model hint 只受 model allowlist 約束，不受 `max_quality` 推導；model registry 沒有可靠的品質
  全序，強行推導會把 routing policy 變成隱藏契約，故保留為已文件化邊界。

## 證據
- `DATABASE_URL=.../apg_w11_final ./scripts/check.sh all` exit 0：frontend 2、Rust 113、
  admin schema 3、usage schema 2、gate selftest 29/29，0 failed／0 skipped；fmt、clippy、build、
  loop、banned constructs、dead files、client contract 與 log secrets 全綠。
- DB 測試：跨 tenant policy 不洩漏；壞 active JSON fail closed；新版啟用會在同交易停用舊版，
  stale CAS 回 409 且現行 active 不受影響；replacement audit 與 post-update row 完全一致；兩個
  concurrent activate 後只有一個 active revision。
- 真實拒絕：未登錄 model 仍先被 active deny 擋下；ledger `rejected`、decision/reason 完整、attempts=0。
- hard ceiling 陰性對照：拿掉 quality clamp 後唯一測試紅，顯示 `Premium != Standard`；還原後綠。
- credential family 陰性對照：改回只查 credential 本身後，輪替 key 得到 `Premium` 而非
  family hard ceiling `Economy`，對應 DB 測試變紅；還原後綠。
- deadline 陰性對照：反轉 provider gate 的逾時條件後，durable rejection 測試變紅；還原後
  ledger status=`rejected`、reason=`deadline_expired`、attempts=0。
- migration 實證：W10 schema 插入當時合法的 201 字 policy key、17 KiB archived config 與 legacy
  workload class，再套 W11 migration 成功，兩筆舊資料均保留；新寫入仍受應用層新上限阻擋。
- 真實 OTLP／路由測試之外，cache 測試證明 warm cache 在 DB 關閉後可用、清除後會重新查 DB，
  並以 capacity 壓力證明 cache 有界且 tenant key 不互串。
- Playwright 真實 gateway 1/1：policy 建立、停用、重啟、封存，viewer 建立 403、operator lifecycle
  403；`.artifacts/w11-policy-{desktop,mobile}.png` 經人工檢視，375px 無溢位或重疊，target 可辨識。
- PR #38：commit `912bc79`，Rust CI 2m46s 與 board sync 10s 均 success；merge `e7bcc4c`。
- `token-ledger.sh` 掃描 19 份 Claude transcript 後以 exit 3 拒絕回報 0；本次主執行是 Codex，
  runtime 未提供精確 session token，故不偽造估算；獨立 reviewer 回報 274,428 tokens。

## 來源
- 規格 §23（分類）、§24（deadline）、§25（來源政策）、§26（合併）、§54（政策後台）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 請求一致性 | evaluate 後凍結 decision，routing 與 ledger 共用同份結果 | provider 前再次查 policy | 避免政策換版使同一請求前後不一致 |
| 合併語意 | defaults 後層覆寫；hard ceiling 取最嚴；model allowlist 取交集 | 單一 priority 數字 | 保留四維語意且 request hint 不能提權 |
| 來源預設 | 掛 Client／Agent identity 的可配置 policy | 解析可偽造、易漂移的 User-Agent | 身分邊界已存在且可稽核 |
| 拒絕紀錄 | provider 前先 durable reserve/finish，attempts 為空 | 只回 403 不留決策 | 能證明拒絕原因與未打上游 |

## 下一步
1. W11 已結束，不再排 tick。
2. 接續 `loops/quota-ratelimit.loop.md`（W12）。
