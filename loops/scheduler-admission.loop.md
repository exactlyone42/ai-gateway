# LOOP: scheduler-admission

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#40](https://github.com/DnaBoss/agent-pool-gateway/issues/40)｜分支 `feature/scheduler-admission`｜段落 `W13`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 待合併 |
| 優先度 | P1 |
| 前置 | `policy-engine`, `quota-ratelimit` |
| 開始 | `2026-09-11T06:52:04+08:00` |
| 結束 | — |
| Token | 主執行為 Codex，Claude ledger exit 3；主代理無法取得精確值，四次 reviewers `758,396` |

## 目的
100 個請求同時進來時，互動式工作不會被背景工作餓死，背景工作也不會永遠排不到。

## 完成判準
- [x] Admission control 在進佇列前判斷七件事：憑證有效／配額／Provider 容量／佇列是否已滿／
      deadline 是否已過期／模型是否允許／預算是否允許。
      決策五種：accept／queue／downgrade／route elsewhere／reject。
- [x] 佇列**有上限**、有 timeout、有 deadline、有明確的拒絕策略。
- [x] Priority class ＋ 加權公平佇列 ＋ **aging**（避免背景永久 starvation）。
      權重起始值：critical 20／interactive 10／foreground 6／automation 3／background 2／batch 1。
- [x] 保留容量：例如 premium 併發 20 = 互動保留 12 ／共用 4 ／背景 4。
      背景塞 1000 個請求**不得**吃掉互動的保留槽。
- [x] 併發上限維度齊全：global／per-provider／per-model／per-user／per-device／per-client／
      per-agent／per-credential／per-workload。
- [x] **禁止** `request → tokio::spawn() → 無上限上游請求`。
- [x] Scheduler 只回答 WHEN；Router 回答 WHERE。兩者不同 module（驗收：程式碼審查 ＋ 依賴方向）。
- [x] Scheduler 後台可見：Active／各 workload 排隊數／各品質等級已用槽位／Provider 併發／
      佇列 P50/P95／被拒數／過期數。
- [x] **快取親和性**：佇列時間超過上游 prompt cache 的 TTL ＝ 100% cache miss。
      沒有錯誤、沒有告警，只有帳單變大——而且打在本專案存在的理由上。
      所以排程決策必須看得到「這個請求的 cache 還有多久過期」，
      且 metric 要**分開記 cache_read 與 cache_write**才量得出代價。
- [x] **陰性對照 1**：關掉 aging → 背景工作在混合負載下必須被觀察到 starvation（測試變紅）。
- [x] **陰性對照 2**：關掉保留容量 → 背景洪水必須把互動 TTFT 拉高（測試變紅）。
      兩條都是「證明這些機制真的在生效」，不是「有寫」。
      ⚠️ **這兩條需要混合負載工具，而那個工具在 W18 階段二**。所以：混合負載的驗收
      掛在 W18 階段二，本段落的完成判準只到「機制存在且可用開關關掉」。
      不要讓一個段落的完成判準依賴排在它後面的段落。

## 已知
- W12 已在 quota admission 實作 tenant/scoped hard/soft 配額與 request-lifetime concurrency lease；
  Scheduler 必須消費其結果，不得另建平行 quota counter。
- 規格 §22 把排程器定位成核心能力：閘道不只是 Router，同時是 AI Compute Scheduler。
- 規格 §27 要求第一版就要有 aging，不能等。
- 規格 §28 把保留容量列為 P0/P1 重要功能。

- W07 移交（2026-09-09）：span 樹缺 `scheduler.queue` 節點（規格 §58）。這一段有排隊實作後補上，作法照 `src/services/chat.rs`。
- 查重：2026-09-11 開工時無同名遠端分支、open PR、對應 open issue 或其它 worktree owner；
  建立唯一卡 #40 與 `feature/scheduler-admission`。
- Reviewer 使用獨立 `codex exec`，計畫與完整 diff 各審一次；高／中風險須修正或以規格與實測裁定。
- `2026-09-11T06:57+08:00` 計畫 reviewer 完成（200,618 tokens）：6 High、5 Medium，全部採納。
  W12 concurrency 不得在 queue 前佔位；九維與 quota constraint 必須一次取得；lease 活到 SSE 結束；
  queue 需 workload/tenant/credential 保留與上限；期限不可由 request 放寬；queue checkpoint 必須 durable。
- 五種 outcome 的 ownership 定案：W13 直接產生 accept/queue/reject；W12 quota 選低品質是 downgrade；
  W17 才產生 reroute。三者由 request admission 流程彙整，Scheduler module 仍只回答 WHEN。
- 公平算法定案：六級固定權重加線性 aging，提升到 critical 權重後以進隊序號決勝；cache expiry
  只在相同 effective priority 內作 tie-break，不能跨 workload 升權。互動/背景 execution 與 queue
  保留槽不可互借；shared 可由兩邊使用，aging 不改 family eligibility。

## 已做
- `2026-09-11T07:09+08:00` 完成 bounded scheduler、六級 smooth weighted fairness＋aging、
  12/4/4 execution 與 64/128/64 queue reservation、九維＋quota atomic lease，接入四條 provider path。
- `2026-09-11T07:09+08:00` queue enter/left/deadline durable checkpoint、取消／timeout／shutdown、
  SSE body-lifetime lease、`scheduler.queue` span、Admin Overview/Governance/System Health 全接通。
- `2026-09-11T07:18+08:00` diff reviewer（233,818 tokens）回 2 High／8 Medium；採納 9 點並修正
  hard/soft mode、crash quota ghost、tenant/credential fair-share、真正 WFQ、runtime switches、cache
  排序、hard deadline 落帳與 fsync 後重查、tenant Admin 隔離。OTLP cache 名稱依工程規範裁定不自創。
- `2026-09-11T07:25+08:00` fresh PostgreSQL `apg_w13_review` 跑 `check.sh all`：Rust 148、schema 5、
  前端 2、關卡自測 29 全綠，0 ignored；production bundle Playwright 桌面／375px 手機無 overflow、
  console error 或 HTTP failure，手機 9 個 nav 全在 8–367px。
- `2026-09-11T07:34+08:00` focused reviewer（162,047 tokens）仍報 1 High／2 Medium；修正 fair-share
  誤逐、enqueue hard quota TOCTOU 與 recovery 二次 crash 非冪等，三個舊失敗狀態各補回歸測試。
- `2026-09-11T07:41+08:00` focused re-review（161,913 tokens）通過，0 High／0 Medium；fresh DB
  `check.sh all` 重跑 Rust 150、schema 5、前端 2、關卡自測 29 全綠，0 ignored。

## 已確定不可行
- 規格 §57 的 13 個 OTLP metric 名稱不在 repo；`docs/engineering-standards.md` 明禁自創同義詞，
  所以本段只交付既有 ledger/Admin 的 cache read/write 分離，不新增未核定 OTLP 名稱。
- W13 不擁有 reroute；route elsewhere 由 W17 實作。W13 直接產生 accept/queue/reject，W12 已產生
  quota downgrade，request admission 流程保留五種 outcome 的 ownership。

## 證據
- `scheduler::tests` 14 條含 20:1 weighted share、aging 20:20、hard reject／soft queue、family 保留、
  blocked-head skip、cache TTL、取消、shutdown、九維原子 release 與 fair-share replacement。
- Crash `.reserved` 或已 rename 的 incomplete `.ready` recovery 都會冪等變 `cancelled`，並寫 quota
  actual token/cost = 0；移除 ready finalize 即失敗。
- 截圖：`.artifacts/w13-scheduler-{desktop,mobile}-final.png`、
  `.artifacts/w13-governance-{desktop,mobile}-final.png`。100-request 混合負載數字依判準移交 W18。

## 來源
- 規格 §22（排程器定位）、§27（加權 ＋ aging）、§28（保留容量）、§29（併發）、
  §32（admission）、§33（分離）、§55（排程後台）、§64（背壓）、§77（排程測試）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| Queue tenant 隔離 | 滿載時替換同 workload 多佔者的最新 waiter | 對未知無限 tenant 靜態預留 | 維持硬上限並讓新 tenant 至少取得 fair share |
| Cache OTLP | 等 §57 正式名稱 | 自創暫時 metric | Metric 名稱是契約，避免永久雙名稱 |

## 下一步
1. 等 focused fix review 與 PR CI 綠後合 `dev`，關閉卡 #40。
2. W18 用兩個 runtime switch 跑 100-request 混合負載，回填 aging／保留容量的 TTFT 數字。
3. 接手 W14 `canonical-layer`；§57 清單取得後回補 cache read/write OTLP 名稱。
