# LOOP: openai-native

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#4](https://github.com/DnaBoss/agent-pool-gateway/issues/4)｜分支 `feature/openai-native`｜段落 `W06`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 已完成 |
| 優先度 | P0 |
| 前置 | `anthropic-native` |
| 開始 | 2026-09-11T00:21+08:00 |
| 結束 | 2026-09-11T01:21+08:00 |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
Codex 與任何 OpenAI 相容 Client 指向閘道就能正常工作，串流／取消／落帳與 Anthropic 那條路同一套骨架。

## 完成判準
- [x] **先做 `POST /v1/responses`**——Codex 的 `wire_api` 只支援 `responses`，
      `/v1/chat/completions` 對 Codex 沒有用。
- [x] 再做 `POST /v1/chat/completions`（給其它 OpenAI 相容 Client）與 `GET /v1/models`。
- [x] 串流／取消／時間戳／ID／落帳**複用** W05 的骨架，不另寫一套
      （驗收方式：`cargo tree` ／程式碼審查確認共用同一個 streaming 模組）。
- [x] **`CanonicalStreamEvent` 在這一段出生**，不等 W14。
      理由：兩個 Provider 一起拉才有辦法設計對；只有 Anthropic 一家時定出來的型別
      一定長得像 Anthropic SSE，W06 就只能把 Responses 掰成那個形狀，或直接分岔。
      其餘 Canonical 型別（Request／Response／Message／Tool*）仍留在 W14。
- [x] `GET /v1/models` 回的清單來自模型註冊表，不是 hardcode 字串。
- [x] 錯誤契約：上游 4xx／5xx 原樣保留 status／body；閘道自有錯誤使用穩定 error code，
      不吞、不一律回 200。
- [x] 串流 contract test：Responses SSE 與 Chat Completions SSE 各一組 fixture。
- [x] **陰性對照**：mock provider 回 malformed SSE（缺 `data:` 前綴／截斷的 JSON）→
      閘道必須回明確錯誤並記 log，**不得**當成正常結束。

## 已知
- 2026-09-11 接手查重：沿用卡 #4 與既有 loop；遠端沒有 `feature/openai-native`，
  沒有同 head 的 open PR；本工作樹已從 `origin/dev` 建立該分支，沒有平行實作。
- 本輪執行者為 Codex；獨立 reviewer 也用 `codex exec --sandbox read-only`，審查意見僅供
  執行者裁定。Claude subscription 目前不可用，不把 reviewer 缺席當成免審理由。
- 計畫定案與完整 diff 各做一次反方審查；高／中嚴重度且不違反既有決策者必須處理。
- 迭代守門 `./scripts/check.sh gate`；PR 前守門 `./scripts/check.sh all`，必須看 exit code，
  另報 passed／skipped 與可證偽的陰性對照，不用 grep 命中代替成功。
- 完成後由 AI 開 PR、合進 `dev` 並驗該 SHA 的 CI；外部依賴先用可回滾的最小路徑打通。
- 每輪記錄開始時間、做什麼、怎麼解決、怎麼證明、耗時；只做安全性、必要依賴與 W06 本身。
- 完整 diff 反方審查提出 2 high、9 medium、2 low，13 點全採納：同 chunk terminal
  先落帳、拒絕 background、最終 checkpoint 可復原、正式失敗 clean EOF、Chat `n` 全 choice、
  metadata 去重、完整 cache usage、端點獨立 body limit、tenant-scoped idempotency key、可調 SSE
  上限、provider 分流、OpenAI secrets gate、修正 4xx 文件契約。
- 現況已有 `/v1/chat/completions`，但走 LiteLLM 且非串流（`src/services/chat.rs:47`）。
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
- **OpenAI 沒有給一般串流的取消端點**：`POST /responses/{id}/cancel` 官方原文
  「Only responses created with the `background` parameter set to `true` can be cancelled.」
  → 非 background 的串流，閘道能做的只有**關掉連線**。
  而且**官方文件對「取消／斷線後是否停止計費」完全沒有說明**（查過 streaming guide、
  background guide、cancel reference 三處）。所以這一段**不得宣稱**取消會省錢。
- **`[DONE]` 哨兵在 Chat Completions 的串流事件文件裡沒有記載**。閘道通常會依賴它 →
  **不要把它當契約**：要能同時處理「有 `[DONE]`」與「沒有 `[DONE]`，靠 `finish_reason` 收尾」。
- Responses 的串流事件帶 `sequence_number`，background 回應可以用 `starting_after` 續接。
  這是未來做「斷線續傳」的入口，但**第一版不做**。

## 已做
- 2026-09-11T00:21+08:00：確認卡 #4、loop、branch ownership 與 open PR，沒有重複工作；
  從最新 `origin/dev` 接手 `feature/openai-native`。
- 2026-09-11T00:29+08:00：完成計畫反方審查；採納 6 high／2 medium。共用 relay 只管理
  完整 SSE event、raw bytes、checkpoint、落帳與取消，協定 observer 才辨識 terminal／usage／error。
  已開始串流後的 malformed 無法改 HTTP status，契約定為中止串流、ledger failed、結構化 log。
- 2026-09-11T00:47+08:00：Responses／Chat 直連、共用串流 relay、CanonicalStreamEvent、
  取消、落帳、穩定錯誤與模型清單完成。Codex CLI 0.153.0 經本機 gateway 與可控 SSE mock
  實際輸出 `W06_CODEX_OK`；client credential 未送到 provider。
- 2026-09-11T00:59+08:00：完整 diff 反方審查完成；逐項重現 2 high、9 medium、2 low，
  沒有以 reviewer 建議取代實測，13 點均完成程式、測試或契約修正。
- 2026-09-11T01:12+08:00：串流與 ledger 收尾：一個 upstream chunk 全部觀察並持久化後才
  yield；final checkpoint 先 durable 再 publish，rename 失敗可在重啟恢復；兩個 provider 分別落帳。
- 2026-09-11T01:21+08:00：PR #32 合入 `dev`（merge `609e651a`）；`rust` CI 與兩個
  board sync check 全部 SUCCESS。Codex 私有 model metadata 缺口移交 W10。
- 2026-09-11 W10 更正移交事實：0.153.0 custom provider 實跑未呼叫 `/v1/models`；官方設定
  以本機 `model_catalog_json` 載入 startup catalog。W06 當時觀察到的 GET 不是該模式的契約。

## 已確定不可行
- 用 Chat Completions 支援 Codex：`wire_api` 目前只接受 Responses。
- 把 OpenAI `response.completed` 與 Chat 的 `finish_reason`／`[DONE]` 當同一種 wire 終止事件：
  Chat 的 usage 可能晚到，Responses 的失敗／不完整又是明確 terminal，合併會漏帳或誤判成功。
- 由 HTTP gateway 消除 Codex custom-provider fallback warning：0.153.0 不向 gateway 查模型，
  只讀本機 `model_catalog_json`；明確模型仍可工作，catalog 要由 operator 另行維護。

## 證據
- `DATABASE_URL=.../gateway_w05_validation ./scripts/check.sh all`：71 passed、0 failed、
  0 ignored（69 lib + 2 schema）；format、Clippy、loops、bans、dead files、secrets 全綠，
  關卡自測 29/29。
- W05 重構前後三條 characterization 都綠；Responses／Chat raw SSE、EOF、malformed、TCP cancel、
  server secret、429/error code 與 PostgreSQL usage/ID 均有獨立測試。
- malformed 陰性對照：暫時把缺 `data:` 改成接受後，指定測試 0 passed／1 failed；恢復後
  1 passed／0 failed，錯誤訊息鎖定 missing data。
- Codex 0.153.0 一次實際往返輸出 `W06_CODEX_OK`；capture 為 `stream=true`、provider
  authorization 僅 server key。W10 重驗 custom provider 只送 `POST /v1/responses`。
- terminal ordering 陰性對照：暫時把 final ledger write 移到第一個 SSE event yield 後，
  指定測試 0 passed／1 failed（spool 沒有 completed）；恢復後 1 passed／0 failed。
- publish fault injection：在 `.ready` 位置建立障礙迫使 rename 失敗，重啟後仍從 final
  checkpoint 恢復 `completed`；證明不了磁碟完全損壞時可寫入。

## 來源
- 規格 §5（端點）、§66（模型發現）。
- <https://developers.openai.com/api/reference/cli/resources/responses/methods/create>
- <https://learn.chatgpt.com/docs/config-file/config-reference>

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 串流驗證粒度 | 暫扣一個完整 SSE event，驗證後原樣送出 | chunk 到即送 | 否則 malformed 或 terminal 落帳失敗只會在成功 bytes 已外洩後才發現 |
| 共用抽象 | relay 管生命週期；observer 管協定狀態 | relay 直接辨識各家 event 名 | 避免表面共用、內部仍按 provider 分支 |

## 下一步
1. W10 提供標準 `/v1/models`；Codex 私有 startup catalog 留在本機 `model_catalog_json`。
