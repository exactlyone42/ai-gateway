# LOOP: openai-native

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 #TBD｜分支 `feature/openai-native`｜段落 `W06`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P0 |
| 前置 | `anthropic-native` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
Codex 與任何 OpenAI 相容 Client 指向閘道就能正常工作，串流／取消／落帳與 Anthropic 那條路同一套骨架。

## 完成判準
- [ ] **先做 `POST /v1/responses`**——Codex 的 `wire_api` 只支援 `responses`，
      `/v1/chat/completions` 對 Codex 沒有用。
- [ ] 再做 `POST /v1/chat/completions`（給其它 OpenAI 相容 Client）與 `GET /v1/models`。
- [ ] 串流／取消／時間戳／ID／落帳**複用** W05 的骨架，不另寫一套
      （驗收方式：`cargo tree` ／程式碼審查確認共用同一個 streaming 模組）。
- [ ] `GET /v1/models` 回的清單來自模型註冊表，不是 hardcode 字串。
- [ ] 錯誤契約：上游 4xx／5xx 對應到穩定的 HTTP status ＋ error code，不吞、不一律回 200。
- [ ] 串流 contract test：Responses SSE 與 Chat Completions SSE 各一組 fixture。
- [ ] **陰性對照**：mock provider 回 malformed SSE（缺 `data:` 前綴／截斷的 JSON）→
      閘道必須回明確錯誤並記 log，**不得**當成正常結束。

## 已知
- 現況已有 `/v1/chat/completions`，但走 LiteLLM 且非串流（`src/gateway.rs:64`）。
- 規格 §66：要區分「閘道支援這個模型」與「Client UI 顯示這個模型」。
  某些 Client 的 model picker 不顯示自訂模型，不代表閘道不支援。
- **Codex 只走 Responses API**：`wire_api` 官方原文「`responses` is the only supported value,
  and it is the default when omitted」（2026-09-08 查證）。所以 `/v1/chat/completions` 不是
  Codex 的路徑，排在 `/v1/responses` 後面。
- Codex 自訂 provider 的可用欄位：`base_url`／`env_key`／`wire_api`／`http_headers`／
  `env_http_headers`／`query_params`／`auth`（指令產生 bearer token，可設 refresh 間隔）／
  `requires_openai_auth`／`stream_idle_timeout_ms`／`stream_max_retries`。
  `auth` 這個欄位對短效憑證是好事，W03 的金鑰輪替可以接上去。
- **已知風險**：Codex **沒有**等同 Anthropic 那份的閘道相容契約文件，所以「它實際會送哪些欄位、
  哪些必須原樣轉送」只能靠實測，不能照抄 Anthropic 那套假設。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §5（端點）、§66（模型發現）。
- OpenAI Responses／Chat Completions 官方文件（URL 待 W01 覆核後填入）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. `/v1/responses` 優先，複用 W05 的 streaming 模組。
2. 用真的 Codex 指向本機閘道錄一次完整請求，比對「文件說的」與「實際送的」差異，寫進
   `docs/provider-protocols.md`。
3. 再補 `/v1/chat/completions` 與 `/v1/models`（接模型註冊表；W10 未完成前先讀設定檔，介面留一致）。
