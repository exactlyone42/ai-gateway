# LOOP: workspace-foundation

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#25](https://github.com/DnaBoss/agent-pool-gateway/issues/25)｜分支 `feature/workspace-foundation`｜段落 `W02`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P0 |
| 前置 | 無 |
| 開始 | `2026-09-10T10:05+08:00` |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
一個指令跑起 PostgreSQL ＋ 閘道 ＋ 可觀測性後端，讓 W03／W04 有資料庫可用。

## 完成判準
- [ ] PostgreSQL 進 `deploy/docker-compose.yml`，port 綁 `127.0.0.1`。
- [ ] migration 綁在**啟動流程**，空資料庫啟動一次就有完整 schema，部署 SOP 不多一步。
- [ ] `GET /health`（行程活著）與 `GET /ready`（依賴就緒）語意分開；
      `/healthz` 保留為 `/health` 的別名，不破壞既有呼叫方。
- [ ] 連線設定走既有的集中設定層，業務邏輯不自己讀環境變數。
- [ ] 資料庫錯誤走既有的統一錯誤出口，不新開一條路。
- [ ] `docker compose up` 一個指令起得來，三個服務都健康。
- [ ] `./scripts/check.sh all` 全綠。
- [ ] **陰性對照**：停掉 PostgreSQL → `/ready` 回非 200，`/health` 仍回 200。
      這條證明兩個端點的語意真的分開，不是同一個東西換名字。

## 已知
- 現況是單 crate，`Cargo.toml` 直接列 12 個依賴，`edition = "2024"`。
- `src/startup.rs:26` 已有 JSON 結構化日誌、`src/routes.rs:23` 已有 `x-request-id` 傳遞層，
  這兩個可直接沿用。
- 現有 `/healthz` 是唯一健康端點，只回 `{"status":"ok"}`，沒有依賴檢查。
- 目前沒有 `sqlx`、沒有 migration、沒有 compose 檔。
- 規格 §86 要求「不要要求開發者手動開十個 Terminal」。

## 已做
- （待填）

## 已確定不可行
- **本段落連 crate 拆分一起做**：反方審查指出 W02 綁了兩個任務，切線在 `sqlx` 進
  `Cargo.toml` 那一刻。同意，而且順序要對調——見下方 Trade-off。

## 證據
- （待填）

## 來源
- 規格 §4（repo 結構）、§56（health 項目）、§63（故障隔離）、§86（本機開發）、§87（secret）。
- Rust workspace 慣例參考：`/home/cash/work_space/private/essence/Cargo.toml`（26 個 crate 的
  `[workspace.dependencies]` 集中版本管理）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 本段落的範圍 | 只做「資料庫可用」 | 同時做 workspace crate 拆分 | 資料庫**擋住 W03／W04**，crate 拆分誰都不擋。先做擋路的那個 |
| crate 拆分的時機 | 移到 W06（第二個協定出現時） | 現在就拆成十幾個 crate | 1684 行、單一開發者，現在拆只是把空目錄先建好；依賴方向的約束要到第二個協定 crate 出現才真的開始付錢。**不為假想的未來需求建目錄** |

## 下一步
1. compose 加 PostgreSQL，port 綁 `127.0.0.1`。
2. `sqlx` 連線池進設定層與啟動流程，migration 在啟動時跑。
3. `/health` 與 `/ready` 分家，`/healthz` 留別名。
4. 陰性對照：停掉資料庫，確認兩個端點反應不同。
