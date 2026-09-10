# LOOP: usage-ledger

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#14](https://github.com/DnaBoss/agent-pool-gateway/issues/14)｜分支 `feature/usage-ledger`｜段落 `W04`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P0 |
| 前置 | `workspace-foundation` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
每一次請求、每一次上游嘗試、每一筆用量都落在 PostgreSQL，且資料庫短暫不可用時不會無聲遺失。

## 完成判準
- [ ] schema：`gateway_requests`（邏輯請求）與 `provider_attempts`（上游嘗試）**分兩張表**，
      一對多；`usage_records`；`audit_logs`。
- [ ] `gateway_requests` 欄位齊全：五個身分外鍵／session_id／trace_id／workload_class／urgency／
      quality／budget_class／requested_model／selected_model／selected_provider／estimated_tokens／
      queue_duration／status／created_at／started_at／completed_at。
- [ ] `provider_attempts` 欄位齊全：provider／model／provider_request_id／provider_response_id／
      attempt_number／started_at／first_token_at／completed_at／status／error_type／
      estimated_usage／actual_usage。
- [ ] estimated 與 provider-reported 用量分開兩組欄位，不互相覆寫。
- [ ] **cache token 分開記**：`cached_input_tokens`（讀）與 cache 寫入分成兩格，
      不要併進 `input_tokens`。理由：排隊超過上游 cache TTL 會讓成本無聲上升
      （見 `docs/architecture.md` 的「排隊會打爛 prompt cache 的經濟學」），
      **這兩格沒分開，帳本天生看不見這筆錢**，W13 的公平性代價就量不出來。
- [ ] `reasoning_tokens` 也單獨一格（不同 Provider 的定義不同，不得相加）。
- [ ] 用量帳本能回答規格 §46 的九個問題（誰／哪台／哪個 Client／哪個 Agent／哪把金鑰／
      哪個模型／哪個 Provider／哪一天／各類 token 與金額）→ 每個問題附一句可跑的 SQL。
- [ ] **有上限**的持久化 outbox：DB 不可用時寫本機 spool，恢復後補寫；spool 滿了要有明確策略
      （拒新請求或降級記錄），**不得無上限記憶體佇列**。
- [ ] 預設**不存** prompt／response 內容，只存 metadata。
- [ ] **陰性對照**：串流進行中 `docker stop` Postgres → 既有串流不得中斷，且用量最終補得回來
      （查 spool → 恢復後查表）。

## 已知
- W02 後 PostgreSQL、啟動時 migration 與 `/ready` 已可用；現況仍沒有用量 schema、
  provider attempt 記錄、`usage_records` 寫入路徑或 outbox。
- 規格 §46 明確：不要用 Prometheus 當帳務資料庫，PostgreSQL 是精確帳本。
- 規格 §42：不同 Provider 的 token 定義不一致（cached input／reasoning tokens），
  不得假設可直接相加。
- 規格 §63：Postgres 故障時，已存在的串流請求不得被 kill；新請求的處理策略要明確。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §42–§46（token 帳務與 schema）、§60（隱私）、§63（故障隔離）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. schema ＋ migration ＋ 九個問題各一句 SQL。
2. 寫入路徑走 outbox，不在請求路徑上同步等 DB。
3. spool 上限與滿載策略。
4. 陰性對照：串流中停掉 DB。
