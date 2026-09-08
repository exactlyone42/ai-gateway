# LOOP: anthropic-native

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 #TBD｜分支 `feature/anthropic-native`｜段落 `W05`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P0 |
| 前置 | `identity-credential`, `usage-ledger` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
Claude Code 指向閘道就能正常工作：串流即時、取消會傳到上游、用量有落帳。

## 完成判準
- [ ] `POST /v1/messages` 與 `POST /v1/messages/count_tokens` 直連 Anthropic，**不經 LiteLLM**。
- [ ] 串流是增量轉送：**不得**先 `bytes().await` 收完整回應再送
      （現況 `src/gateway.rs:64` 就是收完才送，這是要修掉的行為）。
- [ ] Client 斷線／取消／逾時 → 上游請求被取消。
- [ ] 六個時間戳都記：request_received_at／queue_entered_at／queue_left_at／
      provider_request_at／provider_first_byte_at／client_first_byte_at／completed_at，
      並算出 queue latency／provider TTFT／gateway overhead／total latency。
- [ ] 請求 ID 六件齊全：session_id／trace_id／gateway_request_id／attempt_id／
      provider_request_id／provider_response_id。邏輯請求與上游嘗試分開。
- [ ] estimated 與 provider-reported 用量都落帳。
- [ ] 串流 contract test：Provider SSE fixture → 轉送出去的事件序列逐項比對。
- [ ] **陰性對照 1**：把 mock provider 的回應改成「一次吐完」→ 增量轉送測試必須變紅
      （證明測試真的在驗增量，不是只驗最終內容）。
- [ ] **陰性對照 2**：Client 中途斷線 → mock provider 端必須觀察到連線被關閉。

## 已知
- 現況 `/v1/chat/completions` 是非串流整包轉發，且上游是 LiteLLM。這一段要換掉上游並補串流。
- 現況已有 `x-request-id` 傳遞層（`src/main.rs:34`），可當 trace 關聯的起點。
- 規格 §36：不要自己重造 response matching。HTTP／SSE／tokio 本來就維護請求—回應對應，
  application request ID 只用於 trace／用量／重試／稽核／除錯。
- 規格 §40：已開始串流之後不得無腦重試（已產生 token 成本、工具可能有副作用）。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填：`/v1/messages` 官方 SSE 事件形狀、`count_tokens` 回傳欄位，附官方 URL ＋ 查證日期）

## 來源
- 規格 §5（端點）、§35–§42（ID／串流／時間／取消／重試／帳務）。
- Anthropic Messages API 官方文件（URL 待 W01 覆核後填入）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. mock provider（可控 SSE、可控中斷、可控 429／500）先做，再做真實連線。
2. 增量轉送 ＋ 取消傳遞 ＋ 兩個陰性對照。
3. 時間戳與 ID 落帳。
4. 真實 Claude Code 指向本機閘道實測一次往返（回填 W01 的實測欄）。
