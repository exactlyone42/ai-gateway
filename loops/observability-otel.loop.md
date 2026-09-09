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
- [x] 只依賴 OTLP。程式碼不出現 Grafana／Prometheus／Tempo／SigNoz／Datadog 的專屬 API。
      機器檢查：`bans` 第 4 條（字詞邊界比對，`temporary` 不會被誤判），self-test 兩條斷言。
- [~] span 樹照規格 §58：**四個節點已建**（`gateway.request` → `auth.resolve`／`policy.evaluate`／
      `provider.attempt`）。`token.estimate`／`scheduler.queue`／`router.select` 的程式碼還不存在，
      **不先造空 span**（永遠是空的節點會讓人以為那一步被量過）。等 W10／W12／W13 一起補。
- [~] 規格 §57 的 13 個 metric 全部輸出，命名與 OTel semantic 一致。
      **已出兩個** semconv 有定義且現有端點產得出來的：`http.server.request.duration`（histogram，秒）、
      `http.server.active_requests`（up-down counter）。**§57 那份清單不在這個 repo**（只有「13 個」
      這個數字，`docs/engineering-standards.md:229` 說名稱是契約），要規格到手才能對帳。
- [x] **cardinality 規則**：`request_id`／`trace_id`／`session_id` 這類高基數只進 trace／log，
      **不得**當 metric label。白名單 `scripts/metric-labels.txt`（四個標籤）＋ 機器檢查
      （`bans` 第 7 條，self-test 兩條斷言），並用實際送出的 payload 量過（見「證據」）。
- [x] 結構化 JSON 日誌，欄位名一律照 OTel semconv：`timestamp`／`severity_number`／`body`／
      `request_id`／`event.name`／`http.request.method`／`http.route`／`url.path`／
      `http.response.status_code`／`http.server.request.duration`／`error.type`。
      （`trace_id`／`principal`／`gen_ai.*` 前置條件未到，列在 `docs/observability.md`）
- [x] 每個錯誤都被記錄：4xx → severity 13、5xx → severity 17，走同一個出口（存取記錄層）。
- [x] **禁止記錄**：Provider secret、閘道金鑰、密碼、access token、完整 request／response body、
      PII。`./scripts/check.sh secrets` 起真的服務、用哨兵值打四種請求、掃它印出來的每一行。
- [x] OTel 後端不可用時，資料面不受影響（規格 §63）。三種情境實測，見「證據」。
- [x] 日誌每一行都是合法 JSON，連 panic 也是（一行非 JSON 就會讓消費端對整個來源退回文字解析）。
      測試：`logging::tests::a_panic_is_one_json_line`。
- [x] 4xx 記 warning、5xx 記 error，各帶穩定的 `error.type`；5xx 另帶原因。
      測試：`routes::tests::a_rejected_key_is_logged_as_a_warning`／`an_unreachable_upstream_is_logged_with_its_cause`。
- [x] **陰性對照 1**：故意在日誌欄位塞一個假 token 字串 → 遮蔽檢查必須變紅。**做了兩層**：
      掃描器自我對照（`--selftest`）＋ 真的把 `authorization` 標頭記進日誌。兩層都紅（見「證據」）。
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

- reviewer 第二輪（diff 審查）八點的裁定：②③④⑤⑥⑦⑧ 全採納並在 tick 2 修掉；
  **① 駁回**——它建議「同一行同時輸出 pino 與 OTel 兩組鍵」，但 cash 2026-09-09 已決定
  「一種通用格式，消費端做適配，改 aletheia 沒問題」。依相左優先序第一條（使用者既有決策）
  照決策走；我方改為把 aletheia 需要的三個改動寫成明確接口（見 `docs/observability.md`
  的「消費端要改什麼」），含部署順序警告。

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

- `2026-09-09` tick 2：修掉 diff 審查抓到的七個缺口——
  ① 預設等級（`RUST_LOG` 沒設時 `from_default_env()` 只留 ERROR，會讓存取記錄整批消失、
  請求 span 連建都不建）改成程式碼內建 `info,opentelemetry_sdk=warn,opentelemetry_otlp=warn`；
  ② `SIGTERM` 現在也會觸發收工（原本只等 `ctrl_c()`，容器停服時 flush 根本不跑）；
  ③ flush 移到 blocking 執行緒，卡住的後端不再佔用 async worker；
  ④ 502 的原因（DNS／TLS／逾時）帶進 `exception.message`，不再只有一個 `error.type`；
  ⑤ panic 紀錄加 `exception.stacktrace`（`Backtrace::capture()`，尊重 `RUST_BACKTRACE`）；
  ⑥ 測試改用與正式版同一份等級過濾器（原本是平行實作，過濾器錯了測試也綠）；
  ⑦ 修掉兩處講錯的註解／文件（沒設端點時「trace_id 照樣有」是錯的）。
- `2026-09-09` tick 2：補三條測試——4xx 記成 warning ＋ 穩定錯誤型別、5xx 記成 error ＋ 帶原因、
  panic 是一行 JSON。連同既有共 10 條。
- `2026-09-09` tick 2：把 aletheia 端需要的三個改動寫成明確接口（`docs/observability.md`
  的「消費端要改什麼」），含「先部署程式再改設定」的順序警告——順序反了會讓整個觀測層載入失敗。

- `2026-09-09` tick 3：加指標——`http.server.request.duration`（histogram，秒）與
  `http.server.active_requests`（up-down counter），名稱照 semconv。在處理中的計數用 `Drop` guard，
  不是「開始 +1／結束 -1」：Client 中途斷線會讓 future 被丟棄，寫在 `await` 之後的減一永遠不跑，
  那個計數會一路往上爬、看起來像閘道卡住。
- `2026-09-09` tick 3：標籤改走白名單 `scripts/metric-labels.txt`（四個有界標籤）＋ `bans` 第 7 條
  機器檢查（self-test 25 → 27 條）。順手把 rule 5 的正對照 fixture 換成白名單上的標籤，
  否則那條 fixture 會被新規則絆倒、證明不了原本要證明的事。
- `2026-09-09` tick 3：`SIGTERM` 收工時追蹤與指標都會 flush，失敗各自記一筆並標明是哪個信號。

- `2026-09-09` tick 4：加 `./scripts/check.sh secrets`——`scripts/check-log-secrets.sh` 起真的服務、
  用哨兵值當閘道金鑰／上游金鑰／prompt／email，打四種請求（含「對金鑰但上游打不通」，
  那條會記錯誤原因，是最可能夾帶東西的路徑），再掃它印出來的每一行；順便驗每行都是合法 JSON。
  進了 `all`（CI 會跑），並在 `docs/engineering-standards.md` §2 把原本的「驗收動作」換成這個關卡。

## 已確定不可行
- **靜態 grep 當遮蔽檢查**：要抓的不是「程式碼裡有沒有寫 token」，而是執行期的值透過沒人想到的
  欄位流出去（標頭、錯誤訊息、依賴自己印的東西）。實測 tick 4：把 `authorization` 標頭記成一個欄位，
  grep 類關卡全綠，只有跑起來掃輸出的檢查會紅。
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
- **陰性對照（tick 2 的三條，各只紅該紅的）**：① 4xx 改記成 info → 只有
  `a_rejected_key_is_logged_as_a_warning` 紅；② 拿掉錯誤出口塞進 response 的那一行 →
  401 與 502 兩條都紅；③ 預設等級改回 `error` → 200 與 401 兩條紅，502 那條仍綠
  （ERROR 過得去）——這正好說明為什麼光靠 502 的測試抓不到過濾器設錯。
- **實測（tick 2）**：完全不設 `RUST_LOG` → 存取記錄照樣出現（severity 9）；
  送 `SIGTERM` → 收工 flush 真的執行，匯出失敗被記成 severity 17（target `opentelemetry_sdk`）；
  該輪三行輸出全部是合法 JSON。
  → **這證明**預設等級與停服路徑都會留下紀錄；**證明不了**正式環境的容器停服時序（那是 W19）。
- **實測（tick 3，對一個真的會收的 OTLP 端點）**：收到 traces 兩批（1072 ＋ 2185 bytes）＋
  metrics 一批（1219 bytes）。把兩個 signal 的 payload 分開存後比對字串：
  追蹤裡有 `gateway.request`／`auth.resolve`／`policy.evaluate`／`provider.attempt`；
  指標裡有兩個指標名與四個白名單標籤，**沒有** `request_id`／`trace_id`／`span_id`／`exception.message`。
  → **這證明** cardinality 規則在送出的資料上成立，不只是程式碼看起來對；
  **證明不了**指標的數值正確（沒有解 protobuf 比對數字），也證明不了 §57 的 13 個都齊。
- **陰性對照（遮蔽，tick 4 兩層）**：① `--selftest` 往輸出塞一行含哨兵值的紀錄 → 掃描器變紅；
  ② 真的把 `authorization` 標頭記成欄位 → 檢查變紅，而且同時抓出「對的金鑰」與「被拒絕的那把錯金鑰」
  （被拒絕的 token 一樣不能記）。還原後綠。
  → **這證明**掃描器不是橡皮圖章、而且真的攔得住一次程式碼層的洩漏；
  **證明不了** OTLP payload 乾淨（那不在關卡裡，只手動驗過一次）。
- **實測（tick 4，額外）**：用哨兵金鑰與哨兵 prompt 跑一輪並收下 OTLP payload，
  traces 與 metrics 兩個 payload 都不含四個哨兵值中的任何一個（span 全是 `skip_all`）。
- 關卡：`./scripts/check.sh all` 全綠（fmt／clippy --all-targets／10 條測試 0 skipped／
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
1. **接收端 `traceparent` 傳遞**：現在不讀 Client 送來的 W3C 標頭，每個請求都是新 trace，跨系統串不起來。
   要一起做陰性對照：假的 traceparent 不能讓請求失敗。
2. **aletheia 端的三個改動**（另一個 repo、另一張卡）：接口已寫在 `docs/observability.md`，
   要照「先程式後設定」的順序做。
3. **部署後才能做的**：把日誌送進 CloudWatch log group，用
   `{ $.severity_number >= 17 || $.http.response.status_code >= 500 }` 實跑一次
   ——目前只用 `jq` 驗過結構，AWS filter pattern 的語意未驗證。
4. **等別的段落**（含規格 §57 的 13 個指標對帳，要 cash 提供清單）：`token.estimate`／`scheduler.queue`／`router.select` 三個 span（W10／W12／W13）、
   串流不中斷的陰性對照（W05）、`principal`（W03）、`gen_ai.*`（W10）。
