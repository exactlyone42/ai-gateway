# LOOP: observability-otel

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 #TBD｜分支 `feature/observability-otel`｜段落 `W07`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P1 |
| 前置 | `workspace-foundation` |
| 開始 | — |
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
- [ ] 結構化 JSON 日誌，欄位齊全：timestamp／level／trace_id／request_id／attempt_id／
      principal／event／status／provider／model。
- [ ] **禁止記錄**：Provider secret、閘道金鑰、密碼、access token、完整 request／response body、
      PII。驗收：一個掃 log 輸出的檢查（見陰性對照）。
- [ ] OTel 後端不可用時，資料面不受影響（規格 §63）。
- [ ] **陰性對照 1**：故意在日誌欄位塞一個假 token 字串 → 遮蔽檢查必須變紅。
- [ ] **陰性對照 2**：`docker stop` OTel collector → 進行中的串流不得中斷。

## 已知
- 現況 `src/main.rs:23` 已有 `tracing_subscriber` JSON 輸出 ＋ `EnvFilter`，但沒有 OTel exporter、
  沒有 metric、沒有 span 結構。
- 現況 `Cargo.toml` 沒有 `opentelemetry` 相關依賴。
- 規格 §2.3：閘道對可觀測性後端只依賴 OTLP，開發環境可先給 SigNoz compose 因為部署簡單。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §2.3、§38（時間）、§57（metrics）、§58（trace）、§59（log）、§60（隱私）、§63（隔離）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. OTLP exporter ＋ span 樹骨架。
2. 13 個 metric ＋ label 白名單 ＋ cardinality 檢查腳本。
3. 敏感值遮蔽 ＋ 兩個陰性對照。
