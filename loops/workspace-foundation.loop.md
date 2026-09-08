# LOOP: workspace-foundation

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 待開（開工時建；部分覆蓋於 [#1](https://github.com/DnaBoss/agent-pool-gateway/issues/1) [#19](https://github.com/DnaBoss/agent-pool-gateway/issues/19)）｜分支 `feature/workspace-foundation`｜段落 `W02`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P0 |
| 前置 | 無 |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
一個指令跑起 Postgres ＋ 閘道 ＋ 可觀測性後端，且 crate 邊界立好，之後每個段落只動自己的 crate。

## 完成判準
- [ ] 單 crate 拆成 workspace，crate 邊界照規格 §4：`gateway-server` / `gateway-core` /
      `gateway-auth` / `gateway-policy` / `gateway-scheduler` / `gateway-routing` /
      `gateway-accounting` / `protocol-anthropic` / `protocol-openai` / `provider-*`。
- [ ] `cargo tree` 證明依賴方向單向：`gateway-server` → 各 crate → `gateway-core`，**無環**。
- [ ] 設定集中在一處（`gateway-core::config`），業務邏輯不直接讀環境變數。
- [ ] 錯誤統一出口：邊界層一個 `ApiError`，領域層具名錯誤型別。
- [ ] `GET /health`（存活）與 `GET /ready`（依賴就緒）分開；`/healthz` 保留為別名不破壞既有呼叫方。
- [ ] migration 綁在啟動流程（部署 SOP 不多一步）。
- [ ] `docker compose up` 起得來：Postgres ＋ gateway ＋ SigNoz，port 綁 `127.0.0.1`。
- [ ] `scripts/check.sh all` 進 CI，取代目前散在 workflow 的三條指令。
- [ ] **陰性對照**：故意讓 Postgres 停掉 → `/ready` 必須回非 200，`/health` 仍 200。

## 已知
- 現況是單 crate，`Cargo.toml` 直接列 12 個依賴，`edition = "2024"`。
- `src/main.rs:23` 已有 JSON 結構化日誌 ＋ `x-request-id` 傳遞層，這兩個可直接沿用。
- 現有 `/healthz` 是唯一健康端點，只回 `{"status":"ok"}`，沒有依賴檢查。
- 目前沒有 `sqlx`、沒有 migration、沒有 compose 檔。
- 規格 §86 要求「不要要求開發者手動開十個 Terminal」。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §4（repo 結構）、§56（health 項目）、§63（故障隔離）、§86（本機開發）、§87（secret）。
- Rust workspace 慣例參考：`/home/cash/work_space/private/essence/Cargo.toml`（26 個 crate 的
  `[workspace.dependencies]` 集中版本管理）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. 先立 workspace 骨架與依賴方向，**不搬業務邏輯**（行為保持的重構單獨一個 commit）。
2. 再把既有 `/v1/chat/completions` 轉發搬進 `protocol-openai` ＋ `provider-openai`（行為不變）。
3. 加 `/ready` 與依賴檢查、compose、migration 啟動掛勾。
