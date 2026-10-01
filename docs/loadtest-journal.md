# 壓測 journal

**每個效能數字都要在這裡有一段。** 引用一個沒有在這裡登記的數字，等於引用不存在的證據。

規則（`docs/engineering-standards.md` 第 5 節）：

- 先量到數字證明有效，再談測試品質。壓測的本業是量吞吐，不是寫單元測試。
- 效能宣稱要有**前後對照**，不是只有「後」。
- 留「大概」等於沒量。

## 紀錄格式

每一輪一段，六個欄位缺一不可：

```markdown
## YYYY-MM-DD 這一輪想驗什麼（一句）

- **環境**：機器規格、Postgres 版本、gateway git sha、mock provider 設定
- **怎麼跑**：可複製貼上的指令
- **數字**：P50／P95／P99、TTFT、每條串流記憶體、N 條串流的 CPU
- **對照**：改動前 vs 改動後（沒有「前」就寫「首次量測，無對照」）
- **結論**：一句
- **這個數字證明不了什麼**：一句。說不出邊界代表還沒想清楚
```

## 紀錄

## 2026-09-11 驗證 100 條程序內串流狀態與 100 筆混合排程不互串、不漏、不餓死

- **環境**：Linux 7.0.0-31-generic x86_64、AMD RYZEN AI MAX+ 395（32 logical CPU）、
  MemTotal 115011236 KiB、rustc/cargo 1.98.0、gateway `030cd71`。不連 PostgreSQL、HTTP provider
  或 OTLP；mock 是同一程序內的正式 `EventDecoder`／observer／`Scheduler`，SSE event 上限 4096 bytes，
  scheduler active capacity 6、provider limit 3。
- **怎麼跑**：
  `cargo test w18_in_process_load_harness --release --lib -- --nocapture --test-threads=1`。
- **數字**：100 條都先處理正式 `response.created` 才進同步點，觀察到 peak 100；90 條正式完成、
  10 條取消、100 個 response ID 唯一、0 個 ID 錯配、收工 active 0、FD delta 0。程序 CPU
  1,187,490 ns，wall 2,394 us；第一個正式 event 的程序內處理延遲 P50/P95/P99 = 1/2/4 us。
  VmRSS 9,884 -> 9,900 KiB；歷史 VmHWM 增量 16 KiB，向上取整後每條 1 KiB。三輪結束 RSS
  依序 9,532／9,876／9,900 KiB。100 筆 scheduler 全部完成，queue wait P50/P95/P99 =
  20,638/37,910/40,041 us；provider peak 3；前 20 筆有 15 筆 interactive；60 筆
  background/batch 全部完成，最後一筆位置 99。
- **對照**：首次量測，沒有改動前效能數字。機制陰性對照為：provider limit 3 -> 6 時 peak
  3 -> 6；關閉 workload-family reservations 後 background 可吃滿容量且 interactive 進佇列；關閉
  aging 後同一筆 batch 的 dispatch position 1 -> 5；共用兩條 stream 的 observer 會拒絕第二個
  response ID。malformed、partial EOF、oversize event 都被正式 parser 拒絕。
- **結論**：在單一程序的 batch simulation 中，正式 parser/observer/scheduler 可維持 100 個同時
  stream state，取消後資源歸零，provider cap、workload reservations 與 aging 都由對照組證實生效。
- **這個數字證明不了什麼**：這不是 100 次 HTTP/DB/OTLP 呼叫，不能宣稱真實網路吞吐、socket
  backpressure、端到端 TTFT、PostgreSQL/OTLP 在壓力下的容量或任意長時間的記憶體上限。三輪 RSS
  只顯示本次程序趨穩；第一 event latency 不是模型 TTFT。外部邊界故障只用一次性 production-path
  測試驗證，因上位規則禁止對同一遠端服務重複呼叫。

### 同版故障矩陣

| 情境 | 一次性 production-path 證據 | 應有結果 |
| --- | --- | --- |
| Provider idle timeout | `provider_idle_timeout_calls_the_upstream_once_and_returns_502` | 只呼叫一次，上游閒置後 502 |
| Provider 429 | `openai_upstream_rejection_preserves_status_code_body_and_retry_header` | 保留 status/body/retry-after |
| Provider 500 | `transient_failure_routes_the_next_request_without_retrying_the_first` | 本次不重試；下一次改選健康 provider |
| 串流中斷 | `eof_without_message_stop_is_a_stream_error` | 缺 terminal event 即失敗 |
| PostgreSQL 不可用 | `a_database_outage_does_not_change_the_upstream_response_and_recovers` | 已開始的資料面回應不被 ledger 寫入失敗改寫 |
| OTel 不可用 | `docs/observability.md` 的 2026-09-09 三種實測 | exporter 缺席/拒絕/卡住都不影響資料面 |
| Client 斷線 | `dropping_the_client_body_closes_the_provider_stream` | drop 下游 body 會關閉 owned 上游 body，不重試 |

OTel 一列沿用 W07 的既有實測，本輪沒有重打接收端；其餘由上述完整 Rust suite 執行一次。
