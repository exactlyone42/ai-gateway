# LOOP: usage-ledger

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#14](https://github.com/DnaBoss/agent-pool-gateway/issues/14)｜分支 `feature/usage-ledger`｜段落 `W04`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P0 |
| 前置 | `workspace-foundation` |
| 開始 | `2026-09-10T21:49+08:00` |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
每一次請求、每一次上游嘗試、每一筆用量都落在 PostgreSQL，且資料庫短暫不可用時不會無聲遺失。

## 完成判準
- [x] schema：`gateway_requests`（邏輯請求）與 `provider_attempts`（上游嘗試）**分兩張表**，
      一對多；`usage_records`；`audit_logs`。
- [x] `gateway_requests` 欄位齊全：五個身分外鍵／session_id／trace_id／workload_class／urgency／
      quality／budget_class／requested_model／selected_model／selected_provider／estimated_tokens／
      queue_duration／status／created_at／started_at／completed_at。
- [x] `provider_attempts` 欄位齊全：provider／model／provider_request_id／provider_response_id／
      attempt_number／started_at／first_token_at／completed_at／status／error_type／
      estimated_usage／actual_usage。
- [x] estimated 與 provider-reported 用量分開兩組欄位，不互相覆寫。
- [x] **cache token 分開記**：`cached_input_tokens`（讀）與 cache 寫入分成兩格，
      不要併進 `input_tokens`。理由：排隊超過上游 cache TTL 會讓成本無聲上升
      （見 `docs/architecture.md` 的「排隊會打爛 prompt cache 的經濟學」），
      **這兩格沒分開，帳本天生看不見這筆錢**，W13 的公平性代價就量不出來。
- [x] `reasoning_tokens` 也單獨一格（不同 Provider 的定義不同，不得相加）。
- [x] 用量帳本能回答規格 §46 的九個問題（誰／哪台／哪個 Client／哪個 Agent／哪把金鑰／
      哪個模型／哪個 Provider／哪一天／各類 token 與金額）→ 每個問題附一句可跑的 SQL。
- [x] **有上限**的持久化 outbox：DB 不可用時寫本機 spool，恢復後補寫；spool 滿了要有明確策略
      （拒新請求或降級記錄），**不得無上限記憶體佇列**。
- [x] 預設**不存** prompt／response 內容，只存 metadata。
- [ ] **陰性對照**：串流進行中 `docker stop` Postgres → 既有串流不得中斷，且用量最終補得回來
      （查 spool → 恢復後查表）。

## 已知
- 查重（2026-09-10）：沿用卡 #14 與本 loop；GitHub 無 `feature/usage-ledger` 遠端分支，
  也沒有同名開放 PR，已從最新 `origin/dev` 建立既定分支。
- 本輪執行者為 Codex；獨立 reviewer 使用 `claude -p --model opus`，意見由執行者依
  cash 決策、專案守則、技術證據裁定。計畫定案與 diff 完成後各 review 一次。
- 預設 Claude reviewer 因組織停用 subscription access 而無法執行；改用獨立
  `codex exec --sandbox read-only`。計畫審查三點全採納：入口預留 spool 配額、完成時追加
  單一自含 envelope、DB worker 以單一交易展開四表；本機 spool 的容器生命週期風險要明載。
- schema diff 審查找到八點並全採納：usage 的 request/attempt 複合外鍵、每 attempt/source
  冪等鍵、provider-reported 分 Provider/Model 加總、查實際 attempt、不提供未定義 adjustment、
  UTC 明確區間、禁止 TRUNCATE、九條 SQL 改成可直接載入的 PREPARE 並進真 DB 測試。
- 完整 diff 審查五點：雙檔實體預留封閉完成時 ENOSPC、`finish` 在 await 前移交所有權封閉
  Drop 競態、identity 維度禁止硬刪除避免延遲 FK 毒化、outage 測試改驗 response body 與
  首次失敗後第二筆不再觸發 DB；真 SSE 依 W05 邊界先回 501，不冒充已有串流帳務。
- 聚焦複審再找到一高二中並全採納：惡意 request ID 改在 handler 與 ledger 邊界正規化；
  flush 增加單一 pending batch worker，並行完成不漏喚醒；entry 下限固定 32 KiB，所有落帳字串
  以 512 bytes 封頂，且 `finish` 移交所有權前先 encode。
- 每段守門看程序結束碼；完成宣稱必須有陰性對照、passed/skipped 數與真實資料路徑證據。
- 完成後 AI 可自行合入 `dev`；外部依賴先以可回滾的最小路徑打通。
- W02 後 PostgreSQL、啟動時 migration 與 `/ready` 已可用；現況仍沒有用量 schema、
  provider attempt 記錄、`usage_records` 寫入路徑或 outbox。
- 規格 §46 明確：不要用 Prometheus 當帳務資料庫，PostgreSQL 是精確帳本。
- 規格 §42：不同 Provider 的 token 定義不一致（cached input／reasoning tokens），
  不得假設可直接相加。
- 規格 §63：Postgres 故障時，已存在的串流請求不得被 kill；新請求的處理策略要明確。
- W04 開工時只有 non-streaming OpenAI-compatible 路徑；本輪可證明 DB 中斷不改變既有請求
  的 200 回應，但不能把它冒充長命串流證據。W05 交付 SSE 後必須重跑同一故障注入。

## 已做
- 2026-09-10 21:49：查重後從最新 `origin/dev` 建立 `feature/usage-ledger`，卡 #14 無重複
  分支或 PR；完成第一次反方計畫審查。
- 2026-09-10 22:15：四表 migration、append-only／tenant／冪等約束、九條可直接載入的
  PREPARE SQL 與 migration forward/rollback 策略完成；schema diff 審查八點全修。
- 2026-09-10 22:40：完成固定槽位磁碟 outbox、單一 envelope、單次批次 DB function、
  DB 首次失敗即停寫、重啟一次補寫、滿載在上游前拒絕，以及 request/attempt/usage 實際接線。
- 2026-09-10 22:47：依完整 diff 審查重做雙檔 spool 與取消狀態機；補 identity deletion guard、
  `stream:true` 501 邊界、兩側 crash recovery、第二筆停寫與完整 response body 驗證。
- 2026-09-10 22:52：修完聚焦複審一高二中；新增 request ID、pending batch、最小槽位三條測試，
  重跑完整 gate 全綠。

## 已確定不可行
- 在 W05 前把 `stream:true` 的 SSE body 當 JSON 解析：終端 usage event 會變成零且不落
  `usage_records`；W04 先明確 501，W05 實作增量 SSE 後才能移除。
- 只用 `set_len` 預留 completion workspace：檔案可能是 sparse，不能證明有實體磁碟容量；
  改成在上游前實際寫滿零值並驗配置 block 數。

## 證據
- 真 PostgreSQL 套用 migration 後，在單一交易各插入一筆 request／attempt／usage／audit，
  查得 `1/1/1/1`；同一 script 注入 `UPDATE usage_records`，trigger 明確拒絕後 rollback。
- `cargo test --test usage_ledger_schema`：2 passed／0 failed／0 ignored；拆掉 request/attempt
  複合外鍵時精準成為 1 failed／1 passed，復原後 2 passed。
- outbox 測試 2 passed：32 KiB 槽位真的占用磁碟且第二筆拒絕；DB 不可達只留 spool，
  健康 DB 一次補寫後 request/attempt/usage 各一筆，重播同 envelope 不重複。
- route 測試 9 passed：真 HTTP 成功路徑落五類 token；DB 不可達仍回上游的 200，spool
  保留一筆且不含 request prompt／provider response 哨兵；滿載回 503 且上游未被呼叫。
- 陰性注入：移除 `RequestEnvelope::complete` 後 DB 故障恢復測試精準 0 passed／1 failed
  （usage 0≠1）；復原後 1 passed。
- 完整 gate：`./scripts/check.sh all` 為 Rust 32 passed／0 failed／0 ignored、schema 2 passed／
  0 failed／0 ignored、關卡自測 29/29、動態 log secret 掃描無命中。
- 雙檔 spool 各實際配置 32 KiB；startup 在 working 半寫時保留 interrupted，在 ready 已 rename
  但 reserved 未刪時保留 completed。移除 `stream:true` 501 邊界後，目標測試精準由 1 passed
  變 1 failed（實際 200≠501），復原後 1 passed。

## 來源
- 規格 §42–§46（token 帳務與 schema）、§60（隱私）、§63（故障隔離）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| outbox 原子單位 | 一次請求一個自含 envelope | request/attempt/usage 分筆落盤 | 分筆會在程序中斷時留下半套帳，且 FK 寫入順序更複雜 |
| spool 滿載 | 入口先預留固定上限，失敗就拒絕新請求 | 完成後才發現寫不下 | 已完成的請求不能在最後一刻無聲漏帳 |
| DB 恢復 | 首次失敗停止 DB 寫入；修復後重啟一次批次補寫 | 自動 retry loop | 避免反覆連線觸發 rate-limit／封鎖；spool 在 named volume 不隨容器重建消失 |

## 下一步
1. 提交、推送、開 PR；CI 綠後合 `dev`，但卡 #14 保持開放。
2. 接續 W05；真 SSE 完成後回到本 loop 跑 `docker stop` 長命串流陰性對照並封閉 W04。
