# 可觀測性

**這份文件涵蓋日誌、追蹤與指標。** 指標目前是 semconv 定義的子集，還沒與規格 §57 的 13 個對帳
（W07 未完，見 `loops/observability-otel.loop.md`）。

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
| `trace_id`／`span_id` | 來自 span context（W3C 格式：32／16 個十六進位字元）。沒裝追蹤層時**不輸出**，不填零 |

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

## 追蹤（trace）

span 樹目前四個節點，只包含**現在真的有程式碼**的步驟：

```
gateway.request                 存取記錄層（src/middleware.rs）
├── auth.resolve                金鑰驗證（src/auth.rs）
├── policy.evaluate             模型白名單（src/services/chat.rs）
└── provider.attempt            呼叫上游（src/services/chat.rs）
```

規格 §58 還要求 `token.estimate`／`scheduler.queue`／`router.select`——那三段的程式碼還不存在，
**不先造空 span**：一個永遠是空的節點會讓人以為那一步真的被量過。

每個 span 都用 `#[tracing::instrument(skip_all)]`。`skip_all` 不是風格選擇：
`authorize` 的參數帶著呼叫者的金鑰、`forward` 的參數帶著 prompt，span 欄位跟日誌欄位一樣會被寫出去。

### exporter 是可缺席的

`OTEL_EXPORTER_OTLP_ENDPOINT` 沒設 → 完全不裝 exporter，**連 tracer provider 都沒有，所以日誌也沒有
`trace_id`**（只有 `request_id`）。要跨服務串同一個請求就必須設這個變數。
設了但後端掛掉／卡住 → 資料面不受影響（見下方實測）。收工時會 flush（SIGTERM 與 Ctrl-C 都會），
flush 失敗記一筆 error。flush 是阻塞呼叫，跑在 blocking 執行緒上，不佔用 async worker。

### 匯出失敗會出現 severity 17，但那不是資料面的問題

OTLP 送不出去時 SDK 會記 `BatchSpanProcessor.ExportError`（severity 17，target `opentelemetry_sdk`）。
**這是「觀測系統自己壞了」，不是閘道壞了**——告警規則要把 target 是 `opentelemetry_sdk` 的那些分到
另一軸，否則後端維護時會看起來像閘道掛了。

### panic 也是 JSON

依賴函式庫 panic 時，預設會往 stderr 印多行純文字——**一行非 JSON 就會讓消費端對整個來源退回文字解析**。
所以裝了 panic hook，把 panic 寫成一行 JSON（`error.type = "panic"` ＋ 位置 ＋ 訊息）。
這不是假想情境：2026-09-09 實測時 OTLP 批次執行緒真的 panic 了（見下方）。

## 指標（metric）

| 指標 | 型別 | 單位 | 回答什麼 |
| --- | --- | --- | --- |
| `http.server.request.duration` | histogram | 秒 | 延遲分佈、請求數（histogram 自帶 count）、失敗率（按 `error.type` 切） |
| `http.server.active_requests` | up-down counter | 個 | 現在有幾個請求在處理中 |

名稱來自 semconv（`opentelemetry-semantic-conventions-0.27.0/src/metric.rs:1642,1589`）。
**規格 §57 說有 13 個指標，但那份清單不在這個 repo**（只有「13 個」這個數字），
所以先出這兩個 semconv 有定義、現有端點真的產得出來的，其餘等規格到手再對帳。

在處理中的計數用 **guard（`Drop`）** 實作，不是「開始 +1／結束 -1」兩行：Client 中途斷線時
handler 的 future 會被丟棄，寫在 `await` 之後的減一永遠不會執行——那個計數會一路往上爬，
看起來像閘道卡住。

### 標籤走白名單，不是黑名單

允許的標籤只有四個，明列在 `scripts/metric-labels.txt`：
`http.request.method`／`http.response.status_code`／`http.route`／`error.type`。全部是有界集合。

**機器檢查**：`./scripts/check.sh bans` 會把程式碼裡每個 `KeyValue::new("...")` 的鍵拿去比對白名單，
不在名單上就紅（self-test 兩條斷言：名單外的要紅、名單上的要綠）。
為什麼是白名單：加錯一個標籤的後果是幾天後指標後端被時間序列壓垮，離肇事的那次 commit 很遠。

`http.route` 只在真的匹配到路由時才附上——404 沒有 route，硬填一個就把有界標籤變成無界。

## 等級過濾：`RUST_LOG` 沒設也要看得到東西

`EnvFilter::from_default_env()` 在環境變數缺席時**只留 ERROR**
（`tracing-subscriber-0.3.23/src/filter/env/mod.rs:289`）——那會讓存取記錄整批消失、
請求 span 連建都不建（於是沒有 `trace_id`），而輸出看起來完全正常。所以預設值寫在程式碼裡：

```
info,opentelemetry_sdk=warn,opentelemetry_otlp=warn
```

**如果你要自己設 `RUST_LOG`，記得保留 exporter 的 directive。** EnvFilter 一旦有任何 directive
就不再補預設值（`filter/env/builder.rs:335`），漏掉的話「追蹤送不出去」在正式環境沒人會知道。

## 不記敏感值：用跑起來的服務驗，不是用 grep 驗

`./scripts/check.sh secrets` 起一個真的服務，用哨兵值當閘道金鑰、上游金鑰、prompt 內容與 email，
打四種請求（免驗證端點、沒帶金鑰、錯金鑰、對金鑰但上游打不通——最後那條會記錯誤原因，
是最可能夾帶東西的路徑），然後掃它印出來的每一行。任何哨兵值出現即紅。

會漏的東西靜態 grep 抓不到：問題不是「程式碼裡有沒有寫 token」，而是**執行期的值透過沒人想到的
欄位流出去**（標頭、錯誤訊息、依賴函式庫自己印的東西）。

**兩層陰性對照**：
1. `--selftest` 往輸出塞一行含哨兵值的紀錄 → 掃描器必須變紅（證明掃描器本身不是橡皮圖章）。
2. 真的把 `authorization` 標頭記成一個欄位 → 檢查必須變紅。

實測 2026-09-09：兩層都紅，而且指出的是對的那幾行（含「錯的那把金鑰」——被拒絕的 token 一樣不能記）。

**邊界**：只掃日誌。送去 OTLP 的 span／metric payload 不在這個關卡裡；手動驗過一次是乾淨的
（所有 span 都是 `#[tracing::instrument(skip_all)]`，參數不進 span 欄位）。

## 消費端（aletheia）要改什麼

閘道這頭已完成，格式不會再改。SRE 觀測層要讀得到，需要三個改動：

| 檔案 | 改什麼 |
| --- | --- |
| `src/logs.rs:28` | `LogFormat` 加一個 `Otel` variant（編譯器會逼 `parse_entry`／`zero_means_none`／`caveat` 三處補 arm） |
| `src/logs.rs:340-380` | 新 arm 讀：`severity_number`、`trace_id`、巢狀 `http.request.method`／`http.route`／`http.response.status_code`、`error.type`、`body`。`zero_means_none = true`、`caveat = None` |
| `sre/target.yaml` | 加一個 log group：`format: otel`、`error_pattern: '{ $.severity_number >= 17 \|\| $.http.response.status_code >= 500 }'`。要抓 4xx 用 `>= 13` |

**部署順序不能反**：先部署懂 `otel` 的新版程式，再改 `target.yaml`。
設定載入是 `serde_yaml::from_str(...)` ＋ `deny_unknown_fields`（`src/target.rs:231`），
舊版程式讀到 `format: otel` 會直接載入失敗 → 整個觀測層停擺。

## 還沒有的

`principal`（要等 W03 身分）、`gen_ai.request.model`／`gen_ai.system`／`gen_ai.usage.*`
（要等模型註冊表）、**與規格 §57 的 13 個指標對帳**（清單不在 repo）、
以及**接收端的 `traceparent` 傳遞**——目前不讀 Client 送來的 W3C 標頭，每個請求都是新的 trace，
所以跨系統串不起來（要自己的陰性對照：假的 traceparent 不能讓請求失敗）。

## 檔案地圖

| 檔案 | 職責 |
| --- | --- |
| `src/logging.rs` | wire 格式：`FormatEvent` 實作、severity 對應、點號欄位巢狀化、空值丟棄 |
| `src/middleware.rs` | 存取記錄層：一個請求一筆，取 request id／route／狀態／耗時 |
| `src/error.rs` | `error.type` 的定義，以及把它從錯誤出口帶到存取記錄層 |
| `src/startup.rs` | 安裝 subscriber、收工時 flush 追蹤（等級仍由 `RUST_LOG` 決定） |
| `src/config.rs` | `OTEL_EXPORTER_OTLP_ENDPOINT`（沒設就不裝 exporter） |
| `src/metrics.rs` | 兩個指標、在處理中的 guard、標籤只用白名單上的 |
| `scripts/metric-labels.txt` | 標籤白名單（機器檢查讀這份） |
| `scripts/check-log-secrets.sh` | 起服務、打請求、掃輸出有沒有敏感值（含自我陰性對照） |

## 實測（2026-09-09）

| 情境 | 結果 |
| --- | --- |
| 沒設 OTLP 端點 | `/healthz` 200；存取記錄沒有 `trace_id`（符合設計，不填零） |
| 端點沒人在聽 | `/healthz` 200；`BatchSpanProcessor.ExportError` 被記錄（severity 17） |
| 端點接得上但永不回應 | 卡住期間 `/healthz` 仍 200、`/v1/chat/completions` 仍 403；收工 flush 逾時並記一筆 error；**七行輸出全部是合法 JSON** |
| 批次匯出執行緒 panic | **修掉了**：原因是 async HTTP client 跑在 SDK 自己的執行緒上（沒有 Tokio reactor），改用 blocking client。修之前 panic 訊息是三行純文字，直接汙染日誌串流 |
| 完全不設 `RUST_LOG` | 存取記錄照樣出現（severity 9）——預設值在程式碼裡，不靠 `.env` |
| `SIGTERM` 停服 | flush 真的執行了，匯出失敗被記成 severity 17（`opentelemetry_sdk`）。修之前只等 `ctrl_c()`，容器停服時整段不會跑 |
| 對一個**會收的** OTLP 端點跑一輪 | 收到 traces 兩批（1072 ＋ 2185 bytes）與 metrics 一批（1219 bytes）。追蹤裡有 `gateway.request`／`auth.resolve`／`policy.evaluate`／`provider.attempt`；指標裡有兩個指標名與四個白名單標籤，**沒有** `request_id`／`trace_id`／`span_id`／`exception.message` —— cardinality 規則是量出來的，不是宣稱的 |
