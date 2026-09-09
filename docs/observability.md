# 可觀測性

**這份文件現在只涵蓋日誌。** metric 與 trace 還沒做（W07 未完，見 `loops/observability-otel.loop.md`）。

## 日誌格式：一行一個 JSON，OpenTelemetry 命名

實際輸出（2026-09-09 實測，`/healthz` 一筆）：

```json
{"body":"request completed","event":{"name":"http.access"},
 "http":{"request":{"method":"GET"},"response":{"status_code":200},
         "route":"/healthz","server":{"request":{"duration":0.000094124}}},
 "request_id":"b9f191ce-…","severity_number":9,"severity_text":"INFO",
 "target":"agent_pool_gateway::middleware","timestamp":"2026-09-09T07:12:39.703506Z",
 "url":{"path":"/healthz"}}
```

| 欄位 | 來源 |
| --- | --- |
| `severity_number`／`severity_text` | OTLP 日誌模型：TRACE 1、DEBUG 5、INFO 9、WARN 13、ERROR 17（`opentelemetry-proto-0.27.0`，`opentelemetry.proto.logs.v1.rs:189-214`） |
| `http.request.method`／`http.route`／`http.response.status_code` | semconv（`opentelemetry-semantic-conventions-0.27.0/src/attribute.rs:2873,2989,2957`） |
| `http.server.request.duration` | semconv metric 同名，**單位是秒**（`src/metric.rs:1642`） |
| `url.path`、`error.type`、`event.name` | semconv（`attribute.rs:5410,1870,1883`）。`event.name` 的**值** `http.access` 是我們自己取的，不是註冊表項目 |
| `body`、`timestamp`、`target`、`request_id` | 我們自己的欄位；`body` 對應 OTLP 的 `Body` |

## 三個設計決定

1. **`severity_number` 是數字，不是字串。** 現成的 `tracing_subscriber` json formatter 永遠把等級寫成字串
   （`tracing-subscriber-0.3.23/src/fmt/format/json.rs:250`，沒有選項可改），而消費端是用
   `severity_number >= 17` 這種數值條件過濾的。這是自己實作 `FormatEvent` 的唯一理由。
2. **點號欄位名做成巢狀物件。** `tracing` 送過來是扁平的 `"http.route"`，但用 JSON path 過濾的消費端
   （CloudWatch 的 `{ $.http.response.status_code >= 500 }`）定位不到字面上帶點的鍵。
3. **沒有值就不輸出這個欄位。** 空字串會讀成「這個請求沒有 route」而不是「我們不知道」。
   實測：404 那筆沒有 `http.route`（沒有任何路由被匹配到），其餘四筆都有。

## 分級與 `error.type`

| 狀態 | severity | `error.type` |
| --- | --- | --- |
| 5xx | 17 ERROR | 閘道錯誤型別（`upstream_unavailable`、`response_build_failed`） |
| 4xx | 13 WARN | 閘道錯誤型別（`unauthorized`、`model_forbidden`），沒有自己的錯誤時退回狀態碼字串（如 `"404"`） |
| 其餘 | 9 INFO | 不輸出 |

錯誤型別是穩定識別字，**不是訊息文字**——訊息是給人看的，會改。

## 消費端怎麼過濾

```
severity_number >= 17 || http.response.status_code >= 500    # 伺服器端錯誤
severity_number >= 13                                        # 含 4xx
```

實測（2026-09-09，六筆樣本：啟動 ＋ 200／401／403／502／404）：第一條命中 1 筆，就是那筆 502。
**這證明**格式與數值對得上這種過濾；**證明不了** CloudWatch filter pattern 的語意——那要真的送進
log group 才算驗到（判定用 `jq` 做的）。

## 還沒有的

`trace_id`／`span_id`（要等 OTel tracing 接上）、`principal`（要等 W03 身分）、
`gen_ai.request.model`／`gen_ai.system`／`gen_ai.usage.*`（要等模型註冊表）、
13 個 metric、以及日誌遮蔽檢查（不記 secret／PII 的機器驗證）。

## 檔案地圖

| 檔案 | 職責 |
| --- | --- |
| `src/logging.rs` | wire 格式：`FormatEvent` 實作、severity 對應、點號欄位巢狀化、空值丟棄 |
| `src/middleware.rs` | 存取記錄層：一個請求一筆，取 request id／route／狀態／耗時 |
| `src/error.rs` | `error.type` 的定義，以及把它從錯誤出口帶到存取記錄層 |
| `src/startup.rs` | 安裝 subscriber（等級仍由 `RUST_LOG` 決定） |
