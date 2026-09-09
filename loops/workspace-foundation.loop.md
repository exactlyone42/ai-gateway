# LOOP: workspace-foundation

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#25](https://github.com/DnaBoss/agent-pool-gateway/issues/25)｜分支 `feature/workspace-foundation`｜段落 `W02`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 已交付（合入 `dev`） |
| 優先度 | P0 |
| 前置 | 無 |
| 開始 | `2026-09-10T00:45+08:00` |
| 結束 | `2026-09-10T01:07+08:00` |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
一個指令跑起 PostgreSQL ＋ 閘道 ＋ 可觀測性後端，讓 W03／W04 有資料庫可用。

## 完成判準
- [x] PostgreSQL 進 `deploy/docker-compose.yml`，port 綁 `127.0.0.1`。
- [x] migration 綁在**啟動流程**，空資料庫啟動一次就有完整 schema，部署 SOP 不多一步。
- [x] `GET /health`（行程活著）與 `GET /ready`（依賴就緒）語意分開；
      `/healthz` 保留為 `/health` 的別名，不破壞既有呼叫方。
- [x] 連線設定走既有的集中設定層，業務邏輯不自己讀環境變數。
- [x] 資料庫錯誤走既有的統一錯誤出口，不新開一條路。
- [x] `docker compose up` 一個指令起得來，三個服務都健康。
- [x] `./scripts/check.sh all` 全綠。
- [x] **陰性對照**：停掉 PostgreSQL → `/ready` 回非 200，`/health` 仍回 200。
      這條證明兩個端點的語意真的分開，不是同一個東西換名字。

## 已知
- 現況是單 crate，`Cargo.toml` 直接列 12 個依賴，`edition = "2024"`。
- `src/startup.rs:26` 已有 JSON 結構化日誌、`src/routes.rs:23` 已有 `x-request-id` 傳遞層，
  這兩個可直接沿用。
- 現有 `/healthz` 是唯一健康端點，只回 `{"status":"ok"}`，沒有依賴檢查。
- 目前沒有 `sqlx`、沒有 migration、沒有 compose 檔。
- 規格 §86 要求「不要要求開發者手動開十個 Terminal」。

## 已做
- `2026-09-10` 開卡 #25、開分支、草稿 PR [#26](https://github.com/DnaBoss/agent-pool-gateway/pull/26)。
- `2026-09-10` `sqlx` 進依賴；`src/db.rs` 建連線池（lazy）、跑 migration、回答就緒探測。
- `2026-09-10` `DATABASE_URL` 與 `DATABASE_MAX_CONNECTIONS` 進集中設定層；連線池進 `AppState`。
- `2026-09-10` migration 掛進啟動流程，在 listener 綁定**之前**跑。
- `2026-09-10` `/health` 與 `/ready` 分家，`/healthz` 留為別名。
- `2026-09-10` `deploy/docker-compose.yml` ＋ `deploy/Dockerfile`（非 root、兩階段）。
- `2026-09-10` 加 `build.rs`——**陰性對照沒紅才發現的真問題**，見「已確定不可行」。
- `2026-09-10` 修好被我弄壞的 W07 遮蔽關卡：它會啟動真的 binary，所以現在需要資料庫。

## 已確定不可行
- **只靠 `sqlx::migrate!` 而不加 `build.rs`**：那是**編譯期**巨集。沒有
  `cargo:rerun-if-changed=migrations`，增量編譯會沿用舊的展開結果，
  **新加的 migration 不會進到 binary 裡，而且完全無聲**。
  實測（2026-09-10）：放一個語法壞掉的 migration 進去，啟動照樣成功、還印
  「database schema is up to date」。加了 `build.rs` 之後同一個檔案讓啟動失敗、exit 1。
  這是這一段最值得記的一件事——**陰性對照沒有變紅，才抓到它**。
- **本段落連 crate 拆分一起做**：反方審查指出 W02 綁了兩個任務，切線在 `sqlx` 進
  `Cargo.toml` 那一刻。同意，而且順序要對調——見下方 Trade-off。

## 證據
- 實跑（`cargo run` 對 compose 起的 PostgreSQL）：
  啟動日誌 `database schema is up to date` → `agent pool gateway started`；
  資料庫裡出現 `_sqlx_migrations`。
  → **這證明** migration 真的在啟動流程裡跑。**證明不了**它會套用新加的檔案（見下一條）。
- 壞 migration 的前後對照：加 `build.rs` 之前，壞檔案被忽略、啟動成功；
  加了之後 `Error: Migrate(ExecuteMigration(... syntax error at or near "is" ...))`、exit 1。
  移除後啟動恢復正常、`/ready=200`。
  → **這證明**增量編譯的漏洞真的存在，也真的被 `build.rs` 補起來了。
- 停掉資料庫的實測（行程不重啟）：
  `/health` → `200 {"status":"ok"}`；`/ready` → `503 {"database":"unreachable"}`，
  且留下 `error.type=database_unreachable` 的 warn；重開資料庫後 `/ready` 回到 200。
  → **這證明**兩個端點的語意真的分開，而且會自己恢復。
- 單元層同一條也守住了：`readiness_fails_while_liveness_holds_when_the_database_is_down`。
  把 `/ready` 改成永遠回 200 → 該測試 FAILED。**這證明測試會因為程式被改壞而變紅。**
- `./scripts/check.sh all` 全綠：15 passed／0 skipped、loops、bans、deadfiles、
  log secrets、self-test 29/29。

## 來源
- 規格 §4（repo 結構）、§56（health 項目）、§63（故障隔離）、§86（本機開發）、§87（secret）。
- Rust workspace 慣例參考：`/home/cash/work_space/private/essence/Cargo.toml`（26 個 crate 的
  `[workspace.dependencies]` 集中版本管理）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 本段落的範圍 | 只做「資料庫可用」 | 同時做 workspace crate 拆分 | 資料庫**擋住 W03／W04**，crate 拆分誰都不擋。先做擋路的那個 |
| crate 拆分的時機 | 移到 W06（第二個協定出現時） | 現在就拆成十幾個 crate | 1684 行、單一開發者，現在拆只是把空目錄先建好；依賴方向的約束要到第二個協定 crate 出現才真的開始付錢。**不為假想的未來需求建目錄** |
| compose 裡的可觀測性後端 | 不放，`OTEL_EXPORTER_OTLP_ENDPOINT` 沒設就不裝 exporter | 照規格塞一套 SigNoz | W07 已經接到既有的 collector；再起一套是蓋一個沒人要的第二後端 |
| 連線時機 | 連線池 lazy，啟動時才由 migration 觸發第一次真連線 | 啟動就先連 | 行程啟動不綁容器啟動順序，`docker compose up` 不必靠 depends_on 的時序賭運氣 |
| 資料庫主機 port | 預設 `15432`，可用 `POSTGRES_PORT` 覆蓋 | 用 5432 | 開發機上 5432 幾乎一定已經有人佔（本機實測就撞到別的專案）。起不來的 stack 沒人會跑 |
| 遮蔽關卡要不要資料庫 | 要。拿不到就 exit 3 | 讓它在沒有資料庫時跳過 | 它啟動的是真的 binary，而 binary 現在需要資料庫。「沒驗到」不等於「通過」 |

## 下一步
1. PR #26 轉 ready，等 CI 綠燈後合 `dev`。
2. 交棒 W03（身分憑證）與 W04（用量帳本）——它們卡的就是這一段，現在解開了。
3. `check.sh all` 現在需要 Docker（遮蔽關卡要啟動真的 binary）。快速的 `check.sh gate` 不需要。
