# LOOP: observability-otel

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#22](https://github.com/DnaBoss/agent-pool-gateway/issues/22)｜PR [#23](https://github.com/DnaBoss/agent-pool-gateway/pull/23) draft｜分支 `feature/observability-otel`｜段落 `W07`
> 查重（2026-09-09）：`gh issue list` 15 張開放卡無此段落（#19 對到 W02＋W18，見
> `docs/work-breakdown.md:272`）、`git ls-remote` 無同名遠端分支 → 這段沒有人在做，開新卡。

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中（日誌完成，metric／trace 未開始） |
| 優先度 | P1 |
| 前置 | `workspace-foundation` |
| 開始 | 2026-09-09 |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
任何一次請求都查得到「卡在哪一步、花多久、誰發的」，而且換可觀測性後端不用改閘道程式。

## 完成判準
- [ ] 只依賴 OTLP。程式碼不出現 Grafana／Prometheus／Tempo／SigNoz／Datadog 的專屬 API
      （驗收：`grep -ri` 這些名字只出現在 `deploy/` 與文件）。
- [~] span 樹照規格 §58：**四個節點已建**（`gateway.request` → `auth.resolve`／`policy.evaluate`／
      `provider.attempt`）。`token.estimate`／`scheduler.queue`／`router.select` 的程式碼還不存在，
      **不先造空 span**（永遠是空的節點會讓人以為那一步被量過）。等 W10／W12／W13 一起補。
- [ ] 規格 §57 的 13 個 metric 全部輸出，命名與 OTel semantic 一致。
- [ ] **cardinality 規則**：`request_id`／`trace_id`／`session_id` 這類高基數只進 trace／log，
      **不得**當 metric label。驗收：一份明文的 label 白名單 ＋ 一個檢查腳本。
- [x] 結構化 JSON 日誌，欄位名一律照 OTel semconv：`timestamp`／`severity_number`／`body`／
      `request_id`／`event.name`／`http.request.method`／`http.route`／`url.path`／
      `http.response.status_code`／`http.server.request.duration`／`error.type`。
      （`trace_id`／`principal`／`gen_ai.*` 前置條件未到，列在 `docs/observability.md`）
- [x] 每個錯誤都被記錄：4xx → severity 13、5xx → severity 17，走同一個出口（存取記錄層）。
- [ ] **禁止記錄**：Provider secret、閘道金鑰、密碼、access token、完整 request／response body、
      PII。驗收：一個掃 log 輸出的檢查（見陰性對照）。
- [x] OTel 後端不可用時，資料面不受影響（規格 §63）。三種情境實測，見「證據」。
- [x] 日誌每一行都是合法 JSON，連 panic 也是（一行非 JSON 就會讓消費端對整個來源退回文字解析）。
- [ ] **陰性對照 1**：故意在日誌欄位塞一個假 token 字串 → 遮蔽檢查必須變紅。
- [~] **陰性對照 2**：`docker stop` OTel collector → 進行中的串流不得中斷。
      已用「端點接得上但永不回應」代替（比 stop 更嚴格，stop 是連線被拒的 fast fail）；
      **真正的串流還不存在**（現況是整包轉發），要等 W05 才驗得到「進行中的串流不中斷」。

## 已知
- 日誌格式已自己實作（`src/logging.rs`），等級仍由 `RUST_LOG` 決定。沒有 OTel exporter、
  沒有 metric、沒有 span 結構。
- 現成的 `tracing_subscriber` json formatter 的 `level` 永遠是字串
  （`tracing-subscriber-0.3.23/src/fmt/format/json.rs:250`），沒有選項可改成數字。
- 現況 `Cargo.toml` 沒有 `opentelemetry` 相關依賴。
- 規格 §2.3：閘道對可觀測性後端只依賴 OTLP，開發環境可先給 SigNoz compose 因為部署簡單。

- reviewer（`claude -p --model opus`）第一輪的三點：① 計畫只推進一小塊 ② 不加 exporter 更小
  ③ trace_id 靜默變空抓不到。**裁定**：③ 採納（加了端到端斷言）；① 部分採納（實查後有三個節點
  現在就有程式碼，不是單節點）；② 駁回——判準明寫「只依賴 OTLP、span 照 §58」，自己產 id 等於
  自造協定，但採納它的附帶條件「exporter 必須可缺席」。

### 本輪規範（每個 tick 照這份跑）
- **reviewer**：預設 `claude -p --model opus`；意見僅參考，決策在執行者。
- **兩個 review 點**：① 計畫定案（實作前）② `git diff origin/dev...HEAD` 完成後。
- **守門**：迭代中 `./scripts/check.sh gate`；PR 前 `./scripts/check.sh all`（＝CI 全套）。
- **記錄五項**（每段落回寫卡面＋這份 loop）：開始時間／做什麼／怎麼解決／怎麼證明解決／耗時。
- **合併**：AI 可自行合 `dev`；合 `main` 要 CI 全綠 ＋ 本檔完成判準全綠。
- **外部依賴**：優先自己打通（先實測權限再說沒權限），只有「不可逆且無回滾路徑」才停下來問人。
- **只做三類**：安全性／必需依賴／最終目標本身。路上看到的其它問題記進「已知」或另開卡，不撿。

## 已做
- `2026-09-09` 日誌 wire 格式自己實作（`src/logging.rs`）：一行一個 JSON、severity 出數字、
  點號欄位名做成巢狀物件、沒有值就不輸出該欄位。
- `2026-09-09` 加存取記錄層（`src/middleware.rs`）：一個請求一筆，帶 request id／method／route／
  path／狀態／耗時／`error.type`；4xx→13、5xx→17、其餘→9。拿掉 `TraceLayer`（同一件事兩個來源，
  而且它的欄位不是消費端過濾用的那些），`Cargo.toml` 的 `trace` feature 一併移除。
- `2026-09-09` `error.type` 變成穩定識別字（`src/error.rs`），從錯誤出口經 response extension
  帶到記錄層；沒有閘道錯誤的狀態（router 的 404、extractor 擋掉的 body）退回狀態碼字串。
- `2026-09-09` 寫 `docs/observability.md`（欄位表 ＋ 三個設計決定 ＋ 消費端過濾條件 ＋ 檔案地圖），
  `docs/engineering-standards.md` §2 的欄位清單改成 semconv 命名。

- `2026-09-09` 開卡 #22（PAR）、推 `feature/observability-otel`、開 draft PR #23。
- `2026-09-09` OTLP 追蹤接上：exporter 由 `OTEL_EXPORTER_OTLP_ENDPOINT` 決定，沒設就完全不裝；
  span 樹四個節點（`gateway.request` → `auth.resolve`／`policy.evaluate`／`provider.attempt`），
  全部 `skip_all` 免得金鑰與 prompt 進 span；收工 flush，flush 失敗記一筆 error。
- `2026-09-09` 存取記錄多帶 `trace_id`／`span_id`（W3C 格式），沒有 span context 時不輸出、不填零。
- `2026-09-09` 修掉一個實測抓到的 panic：async HTTP client 跑在 SDK 自己的執行緒上會炸，
  改 blocking client；另外裝 panic hook 把 panic 寫成一行 JSON，避免汙染日誌串流。
- `2026-09-09` 依賴函式庫的事件沒有 message 時，`body` 改用它的 `name` 欄位（原本會是 `null`）。

- `2026-09-09` **未完的一件**：diff 的反方審查（`claude -p --model opus`）在 tick 1 收工時還沒回，
  下一個 tick 先讀它、有高／中嚴重度就先修再做指標。

## 已確定不可行
- **OTLP 用 async HTTP client（`reqwest-client`）**：SDK 的批次處理器跑在自己的 std 執行緒上，
  沒有 Tokio reactor → 該執行緒 **panic**（`there is no reactor running`），之後 span 全丟，
  而且 panic 訊息是三行純文字直接汙染日誌串流（2026-09-09 實測）。要用 `reqwest-blocking-client`。
- **`tracing-bunyan-formatter`**（2026-09-09 實接實測）：level 出數字、欄位扁平這兩點對，
  但鍵名照呼叫點原樣輸出（要對上 pino 契約就得把 camelCase 寫進程式碼）、`msg` 被硬加
  `[REQUEST - EVENT]` 前綴（寫死在 formatter，`skip_fields` 改不到）、span 生命週期讓每個請求變 3 行、
  span 與 event 同名欄位會產生**重複鍵** `{"requestId":"x","requestId":"x"}`。
- **`tracing_subscriber` 內建 json formatter**：`flatten_event(true)` 能把欄位提到頂層，
  但 `level` 永遠是字串（`tracing-subscriber-0.3.23/src/fmt/format/json.rs:250`），沒有選項可改成數字。
- **從 log 推 metric**（CloudWatch metric filter 那條路）：可行但不採用——會把 metric 的正確性
  綁在 log 文字上，改一句訊息就可能讓告警靜音。服務自己出 OTLP metric。

## 證據
- 欄位名與 severity 數值都有本地權威來源：`opentelemetry-proto-0.27.0`
  （`opentelemetry.proto.logs.v1.rs:189-214`：INFO 9／WARN 13／ERROR 17）、
  `opentelemetry-semantic-conventions-0.27.0/src/attribute.rs:2873,2957,2989,5410,1870,1883`、
  `src/metric.rs:1642`（duration 單位是秒）。
  → **這證明**鍵名不是我自己編的；**證明不了** `event.name` 的**值** `http.access` 有註冊表依據（沒有）。
- 實測（2026-09-09，起服務打 200／401／403／502／404）：六筆輸出全部是合法 JSON，
  每筆都有 `request_id`；401 帶 `error.type=unauthorized`、403 `model_forbidden`、
  502 `upstream_unavailable`、404 退回 `"404"` 且**沒有** `http.route`（沒有路由被匹配到）。
  → **這證明**分級與空值規則照設計運作；**證明不了** CloudWatch filter pattern 的語意。
- 消費端過濾判定（`jq`，同一份六筆樣本）：`severity_number >= 17 or http.response.status_code >= 500`
  命中 1 筆，正是那筆 502。
- **陰性對照（formatter 的三條測試會紅）**：把 ERROR 的 severity 從 17 改成 pino 的 50 →
  只有 `severity_is_a_number_not_a_string` 紅（left 50／right 17）；關掉點號巢狀化 →
  只有 `semantic_convention_names_become_nested_objects` 紅。兩次都是對的那一條紅，其餘綠。

- **陰性對照（追蹤）**：把 `gateway.request` span 拿掉 → 只有
  `routes::tests::every_request_is_logged_with_a_trace_id` 紅，訊息印出那一行少了 `trace_id`。
  這正是 reviewer 指的靜默失敗：欄位空掉會被 formatter 丟棄，日誌看起來完全正常。
- **陰性對照（後端不可用，三種）**：① 沒設端點 → `/healthz` 200、存取記錄無 `trace_id`；
  ② 端點沒人聽 → 200，且 `BatchSpanProcessor.ExportError` 有被記錄（severity 17）；
  ③ 端點接得上但永不回應 → 卡住期間 `/healthz` 仍 200、`/v1/chat/completions` 仍 403，
  收工 flush 逾時並記一筆 error，**七行輸出全部是合法 JSON**。
  → **這證明**匯出通道壞掉不影響資料面；**證明不了**「進行中的串流不中斷」（串流還不存在，W05 才有）。
- 關卡：`./scripts/check.sh all` 全綠（fmt／clippy --all-targets／7 條測試 0 skipped／
  loop 檔／禁用構件／死檔／關卡自測 25 條）。

## 來源
- 規格 §2.3、§38（時間）、§57（metrics）、§58（trace）、§59（log）、§60（隱私）、§63（隔離）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 日誌格式 | 自己實作 `FormatEvent` | 現成的 `tracing_subscriber` json、`tracing-bunyan-formatter` | 前者的 `level` 只能是字串；後者鍵名照呼叫點原樣輸出（要對上 pino 契約就得把 camelCase 寫進程式碼），還會加 `msg` 前綴、每請求 3 行、span 與 event 同名欄位產生重複鍵（實測） |
| 欄位命名 | OTel semconv | SRE 觀測層那份 pino 方言（`requestId`／`statusCode`） | 一種通用格式讓 ELK／Loki／OTLP collector 都能吃；消費端做適配比來源維護兩種方言少一份漂移 |
| 巢狀 vs 扁平 | 巢狀物件 | 字面帶點的扁平鍵 | 用 JSON path 過濾的消費端定位不到帶點的鍵 |
| 存取記錄 | 自己一層 `from_fn` | `tower_http::TraceLayer` | 它的欄位不是消費端過濾用的那些，留著會讓同一件事有兩個來源 |
| 缺值 | 不輸出該欄位 | 填空字串／`unknown` | 空值會讀成「這個請求沒有 route」而不是「我們不知道」 |

## 下一步
1. **13 個 metric ＋ label 白名單 ＋ cardinality 檢查腳本**：白名單寫成明文檔，腳本進
   `./scripts/check.sh`，並補 self-test 兩條斷言（現有 25 條要跟著加）。
   **先要解一個依賴**：規格 §57 的 13 個 metric 清單不在這個 repo 裡（loop 只引編號）。
   照 cashloop「需人拍板但不涉重大風險 → 有最佳實踐就照做」，我會先用 OTel HTTP semconv
   的 `http.server.request.duration`（histogram）＋ 請求數／錯誤數／併發數起一份最小集合，
   把清單寫進 `docs/observability.md` 並標明「待與規格 §57 對帳」。
2. **敏感值遮蔽 ＋ 陰性對照 1**：掃 log 輸出的檢查；在欄位塞假 token 必須變紅。
3. **接收端 `traceparent` 傳遞**：現在不讀 Client 送來的 W3C 標頭，每個請求都是新 trace，跨系統串不起來。
   要一起做陰性對照：假的 traceparent 不能讓請求失敗。
4. **部署後才能做的**：把日誌送進 CloudWatch log group，用
   `{ $.severity_number >= 17 || $.http.response.status_code >= 500 }` 實跑一次
   ——目前只用 `jq` 驗過結構，AWS filter pattern 的語意未驗證。
5. **等別的段落**：`token.estimate`／`scheduler.queue`／`router.select` 三個 span（W10／W12／W13）、
   串流不中斷的陰性對照（W05）、`principal`（W03）、`gen_ai.*`（W10）。
