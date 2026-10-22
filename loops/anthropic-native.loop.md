# LOOP: anthropic-native

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 無獨立卡（既有 loop 接手）｜分支 `feature/anthropic-native`｜段落 `W05`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 已交付 |
| 優先度 | P0 |
| 前置 | `identity-credential`, `usage-ledger` |
| 開始 | `2026-09-10T23:00+08:00` |
| 結束 | `2026-09-11T00:14+08:00` |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
Claude Code 指向閘道就能正常工作：串流即時、取消會傳到上游、用量有落帳。

## 完成判準
- [x] `POST /v1/messages` 與 `POST /v1/messages/count_tokens` 直連 Anthropic，**不經 LiteLLM**。
- [x] 串流是增量轉送：**不得**先 `bytes().await` 收完整回應再送
      （現況 `src/services/chat.rs:47` 就是收完才送，這是要修掉的行為）。
- [x] Client 斷線／取消／逾時 → 上游請求被取消。
- [x] 時間戳欄位**全部先開好**：request_received_at／queue_entered_at／queue_left_at／
      provider_request_at／provider_first_byte_at／client_first_byte_at／completed_at。
      **W13 之前沒有佇列**，所以兩個 queue 欄位這一段一律寫 NULL，
      並算出 provider TTFT／gateway overhead／total latency 三個現在算得出來的。
      不要為了「看起來有」而填 0——0 和「沒有佇列」是兩件事。
- [x] 請求 ID 六件齊全：session_id／trace_id／gateway_request_id／attempt_id／
      provider_request_id／provider_response_id。邏輯請求與上游嘗試分開。
- [x] estimated 與 provider-reported 用量都落帳。
- [x] 串流 contract test：Provider SSE fixture → 轉送出去的事件序列逐項比對。
- [x] **官方閘道契約**（`docs/feasibility.md` 第二節，每條都是硬要求）：
      路徑比對用 path 不用整個 URL（實際打的是 `/v1/messages?beta=true`）；
      `anthropic-beta`／`anthropic-version` 逐字轉送且**不做白名單**；
      SSE `ping` 與註解行**必須轉送**（靜默 300 秒 Client 會中止串流）；
      上游錯誤本體**原樣轉送**（Client 的自動降級靠比對錯誤字串）；
      `system` 陣列與 `cache_control` 標記原樣轉送、attribution block 保持在第一個獨立區塊；
      `HEAD /api/hello` 預熱探測可拒絕但不能 5xx 噴爆。
- [x] `GET /v1/models?limit=1000` 3 秒內回、**不重導向**，回 `data[]` 含 `id`
      （`display_name`／`description` 可選）。
      **來源是設定檔，不是 W10 的資料表**——W10 只換來源，不換這個端點的回應形狀。
      端點的 contract test 在這裡就定死，W10 必須照著過。
- [x] Principal 解析吃 Client 送來的 `x-claude-code-session-id`（session 關聯）與
      `x-claude-code-agent-id`／`x-claude-code-parent-agent-id`（Agent 歸屬）。
      **agent id 不得當成人的識別**（官方明文）。
- [x] **陰性對照 1**：在 gateway 注入「terminal 前不 yield」→ Provider barrier 測試必須變紅
      （Provider 一次吐完本來就無法證明 proxy buffer，是錯誤對照，已更正）。
- [x] **陰性對照 2**：Client 中途斷線 → mock provider 端必須觀察到連線被關閉。
      **只驗到這裡就停**：上游收到中斷後是否停止生成與計費是 Provider 的行為，
      官方文件沒有明文，所以**不宣稱**。

## 已知
- 查重（2026-09-10）：本 loop 已存在，依 cashloop 接手模式不另開卡；GitHub 無同名遠端分支、
  無開放 PR，現有開放 issue 也沒有 Anthropic-native 實作卡。分支從已合 W04 的最新
  `origin/dev` 建立。
- 本輪執行者為 Codex；Claude reviewer 因組織停用 subscription access 不可用，兩個 review
  點改用獨立 `codex exec --sandbox read-only`，只作反方意見，由執行者依證據裁定。
- 計畫反方審查已完成，十點全部採納：同時支援 Bearer／`x-api-key` 且不把 Client 憑證送上游；
  access log 與 active metric 延後到 response body EOF／錯誤／Drop；串流每次向 Client 交付
  新用量前先 checkpoint；新增欄位保持舊 spool 可解碼；增量測試用 Provider barrier，不以
  TCP chunk 數冒充 SSE 增量；bounded SSE parser 對超長／CRLF／多行／切半 UTF-8 fail-open；
  request 用 raw bytes 轉送、保留 query；明確定義 `client_first_byte_at` 是交給 Hyper 的時間；
  count_tokens 不產生可計費用量；models 來源必須是設定且對不可探索名稱發出警告。
- 現況 `/v1/chat/completions` 是非串流整包轉發，且上游是 LiteLLM。這一段要換掉上游並補串流。
- 現況已有 `x-request-id` 傳遞層（`src/middleware.rs:30`、`src/routes.rs:46`），可當 trace 關聯的起點。
- 規格 §36：不要自己重造 response matching。HTTP／SSE／tokio 本來就維護請求—回應對應，
  application request ID 只用於 trace／用量／重試／稽核／除錯。
- 規格 §40：已開始串流之後不得無腦重試（已產生 token 成本、工具可能有副作用）。
- diff 反方審查 2 高 8 中全部處理：checkpoint 改成雙工作槽原子替換；migration rollback
  前置拆成 PR #30；保留 content-encoding；首 byte 分時量測；收到時間移到 auth/body 前；
  無 exporter 仍建立 trace；message_stop 主動收束、缺失時 body 報錯；支援 CR-only SSE；
  secret gate 走 Messages/x-api-key；取消測試改走真 TCP listener。

- W07 移交（2026-09-09）：`docs/observability.md` 的「陰性對照 2」需要真的長命串流才驗得到（`docker stop` OTel collector 時串流不得中斷）。這一段做出串流後要把那條對照補上。

## 已做
- 2026-09-10 23:00：完成 issue／loop／分支／PR 查重，從 `origin/dev` 建立既定分支。
- 2026-09-10 23:28：完成 direct Anthropic raw proxy、Bearer／`x-api-key` 雙認證、models／hello、
  bounded SSE metadata parser、增量轉送與 Client Drop 取消；新增 response-body lifecycle，access
  log 與 active metric 不再在 headers ready 時提早結算。
- 2026-09-10 23:28：帳務新增七個時間點、Agent header、operation、provider TTFT／gateway overhead／
  total latency；stream first byte／usage／terminal 在交付 Client 前 fsync checkpoint；舊 spool
  缺新增欄位仍可讀，完整 working checkpoint 可在程序中斷後恢復。
- 2026-09-10 23:28：barrier contract test 證明第一批 SSE 在 Provider 放行第二批前已到 Client；
  raw query/header/body/SSE bytes 逐項相同；Client 丟 body 後 mock Provider 觀察到連線關閉。
- 2026-09-10 23:28：fresh PostgreSQL 實測 request/attempt/provider 六種 ID、queue NULL、七時間點、
  estimated 與 provider-reported 分列；`count_tokens` 有 request/attempt 但 usage records 為 0。
- 2026-09-11 00:00：修完 diff 審查 10 點；新增 torn checkpoint、CR-only、缺 message_stop、
  gzip header、真 TCP disconnect 對照；新增 `docs/provider-protocols.md`。
- 2026-09-11 00:04：Claude Code 2.1.267 真 Client 經本機 gateway 到 SSE mock，精確輸出
  `W05_CLAUDE_CODE_OK`。增量負面注入後 barrier 測試精準以 `Elapsed` 變紅，移除後恢復。

## 已確定不可行
- 把 TCP／HTTP chunk 數當成 SSE 增量證據：transport 可任意重切，改用 Provider barrier。
- 在 response header 回傳時結算 access log／active metric：長串流會被誤報成已完成。
- 只在 EOF 寫帳：Client 取消或程序崩潰會遺失 attempt 與已收到的部分 usage。
- 「mock Provider 一次吐完整 body」當增量負面對照：正確 proxy 只能立刻轉送收到的單一 chunk，
  這個情境無法區分 proxy 是否 buffer；改在 gateway 注入 terminal 前不 yield。

## 證據
- 「Claude Code reads the stream as it arrives, so if your gateway buffers complete responses
  before relaying them, Claude Code stalls.」
  → **這證明**現況的整包轉發（`src/services/chat.rs:47`）會讓 Client 卡住，不是效能問題而是壞掉。
- 「Claude Code counts every byte your gateway relays, including SSE `ping` events and comment
  lines, and aborts a stream that goes silent for 300 seconds by default.」
  → **這證明**轉送 ping 是功能需求。**證明不了**上游一定會送 ping（Bedrock 二進位事件流就不送）。
- Claude Code 2.1.267 實測：`claude -p --model claude-test --max-turns 1` 經本機 gateway
  與 Anthropic SSE mock，輸出精確為 `W05_CLAUDE_CODE_OK`；沒有正式 Provider key，故不宣稱
  真 Anthropic 帳號、模型或計費已驗證。
- mock Provider 實測：`/v1/messages?beta=true&x=a%2Fb&x=2` 的 query、開放 Anthropic headers、
  `system`／`cache_control` raw body、ping／comment／事件 bytes 原樣往返；Client Drop 在 1 秒內
  讓 Provider body guard Drop。
- 真 PostgreSQL 實測：兩種 usage 分列，provider response id 從 `message_start` 落帳，queue 欄位
  為 NULL，`count_tokens` 不產生 usage。
- 隔離 DB 完整 gate：Rust 46 passed／0 failed／0 ignored、schema 2 passed／0 failed／0 ignored、
  關卡自測 29/29、動態 secret 掃描無命中且預設無 OTLP exporter 仍有 32 位 trace ID。
- rollback 前置 PR #30 已合；舊版相容 binary 對已套 W05 migration 的 DB 跑完整 gate：
  Rust 32 + schema 2 + 自測 29/29 全綠。部署時必須先上 #30，再上 W05。

## 來源
- 規格 §5（端點）、§35–§42（ID／串流／時間／取消／重試／帳務）。
- <https://code.claude.com/docs/en/llm-gateway-protocol>（閘道相容契約）2026-09-08。
  **這一段的完成判準大半直接來自這份文件，不是自己推的。**
- <https://platform.claude.com/docs/en/api/messages>（Messages API 格式）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 串流帳務 | 交付新 usage／terminal event 前 fsync checkpoint | 只在 EOF 才記一次 | Client 中斷或程序崩潰也要留下 attempt 與已看到的部分用量 |
| SSE 解析 | 有界、只旁讀 metadata、解析失敗仍原樣轉送 | 把 parser 當 protocol gateway | 相容性要求 raw bytes 不變，帳務解析不能反過來拖垮串流 |
| 首 byte 定義 | `client_first_byte_at` = gateway 將第一段 body 交給 Hyper | 宣稱遠端 Client 已收到 | 伺服器無法觀察遠端 socket 真正收件時間 |

## 下一步
接續 W06 `openai-native`，抽出並複用本段的串流／取消／落帳骨架。
