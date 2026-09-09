# LOOP: observability-otel

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 待開（開工時建）｜分支 `feature/observability-otel`｜段落 `W07`

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
- [ ] span 樹照規格 §58：`gateway.request` → `auth.resolve`／`policy.evaluate`／`token.estimate`／
      `scheduler.queue`／`router.select`／`provider.attempt`（→ connect／request／first_token／complete）。
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
- [ ] OTel 後端不可用時，資料面不受影響（規格 §63）。
- [ ] **陰性對照 1**：故意在日誌欄位塞一個假 token 字串 → 遮蔽檢查必須變紅。
- [ ] **陰性對照 2**：`docker stop` OTel collector → 進行中的串流不得中斷。

## 已知
- 日誌格式已自己實作（`src/logging.rs`），等級仍由 `RUST_LOG` 決定。沒有 OTel exporter、
  沒有 metric、沒有 span 結構。
- 現成的 `tracing_subscriber` json formatter 的 `level` 永遠是字串
  （`tracing-subscriber-0.3.23/src/fmt/format/json.rs:250`），沒有選項可改成數字。
- 現況 `Cargo.toml` 沒有 `opentelemetry` 相關依賴。
- 規格 §2.3：閘道對可觀測性後端只依賴 OTLP，開發環境可先給 SigNoz compose 因為部署簡單。

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

## 已確定不可行
- （待填）

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
1. OTLP exporter ＋ span 樹骨架（`trace_id`／`span_id` 進日誌要靠這個）。
2. 13 個 metric ＋ label 白名單 ＋ cardinality 檢查腳本。
3. 敏感值遮蔽 ＋ 兩個陰性對照。
