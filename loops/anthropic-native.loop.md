# LOOP: anthropic-native

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 待開（開工時建）｜分支 `feature/anthropic-native`｜段落 `W05`

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
- [ ] 時間戳欄位**全部先開好**：request_received_at／queue_entered_at／queue_left_at／
      provider_request_at／provider_first_byte_at／client_first_byte_at／completed_at。
      **W13 之前沒有佇列**，所以兩個 queue 欄位這一段一律寫 NULL，
      並算出 provider TTFT／gateway overhead／total latency 三個現在算得出來的。
      不要為了「看起來有」而填 0——0 和「沒有佇列」是兩件事。
- [ ] 請求 ID 六件齊全：session_id／trace_id／gateway_request_id／attempt_id／
      provider_request_id／provider_response_id。邏輯請求與上游嘗試分開。
- [ ] estimated 與 provider-reported 用量都落帳。
- [ ] 串流 contract test：Provider SSE fixture → 轉送出去的事件序列逐項比對。
- [ ] **官方閘道契約**（`docs/feasibility.md` 第二節，每條都是硬要求）：
      路徑比對用 path 不用整個 URL（實際打的是 `/v1/messages?beta=true`）；
      `anthropic-beta`／`anthropic-version` 逐字轉送且**不做白名單**；
      SSE `ping` 與註解行**必須轉送**（靜默 300 秒 Client 會中止串流）；
      上游錯誤本體**原樣轉送**（Client 的自動降級靠比對錯誤字串）；
      `system` 陣列與 `cache_control` 標記原樣轉送、attribution block 保持在第一個獨立區塊；
      `HEAD /api/hello` 預熱探測可拒絕但不能 5xx 噴爆。
- [ ] `GET /v1/models?limit=1000` 3 秒內回、**不重導向**，回 `data[]` 含 `id`
      （`display_name`／`description` 可選）。
      **來源是設定檔，不是 W10 的資料表**——W10 只換來源，不換這個端點的回應形狀。
      端點的 contract test 在這裡就定死，W10 必須照著過。
- [ ] Principal 解析吃 Client 送來的 `x-claude-code-session-id`（session 關聯）與
      `x-claude-code-agent-id`／`x-claude-code-parent-agent-id`（Agent 歸屬）。
      **agent id 不得當成人的識別**（官方明文）。
- [ ] **陰性對照 1**：把 mock provider 的回應改成「一次吐完」→ 增量轉送測試必須變紅
      （證明測試真的在驗增量，不是只驗最終內容）。
- [ ] **陰性對照 2**：Client 中途斷線 → mock provider 端必須觀察到連線被關閉。
      **只驗到這裡就停**：上游收到中斷後是否停止生成與計費是 Provider 的行為，
      官方文件沒有明文，所以**不宣稱**。

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
- 「Claude Code reads the stream as it arrives, so if your gateway buffers complete responses
  before relaying them, Claude Code stalls.」
  → **這證明**現況的整包轉發（`src/gateway.rs:64`）會讓 Client 卡住，不是效能問題而是壞掉。
- 「Claude Code counts every byte your gateway relays, including SSE `ping` events and comment
  lines, and aborts a stream that goes silent for 300 seconds by default.」
  → **這證明**轉送 ping 是功能需求。**證明不了**上游一定會送 ping（Bedrock 二進位事件流就不送）。
- （待填：本機實測 Claude Code → 自架閘道往返一次）

## 來源
- 規格 §5（端點）、§35–§42（ID／串流／時間／取消／重試／帳務）。
- <https://code.claude.com/docs/en/llm-gateway-protocol>（閘道相容契約）2026-09-08。
  **這一段的完成判準大半直接來自這份文件，不是自己推的。**
- <https://platform.claude.com/docs/en/api/messages>（Messages API 格式）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. mock provider（可控 SSE、可控中斷、可控 429／500）先做，再做真實連線。
2. 增量轉送 ＋ 取消傳遞 ＋ 兩個陰性對照。
3. 時間戳與 ID 落帳。
4. 真實 Claude Code 指向本機閘道實測一次往返（回填 W01 的實測欄）。
