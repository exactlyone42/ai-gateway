# 目錄結構地圖

新增檔案前先看這份：每個目錄放什麼、不放什麼。`./scripts/check.sh layout` 會檢查根目錄與 `src/` 第一層，沒列在這裡的目錄過不了閘門。

## 根目錄

| 目錄 | 放什麼 | 不放什麼 |
| --- | --- | --- |
| `src/` | 閘道的 Rust 程式（見下表） | 腳本、文件、測試資料 |
| `tests/` | Rust 整合測試（`*.rs`）、測試用 SQL（`*.sql`）、固定資料（`fixtures/`） | 正式程式、一次性腳本 |
| `migrations/` | 資料庫 schema 遷移，只往前加，不改舊檔 | 查詢、測試資料 |
| `queries/` | 編譯期檢查的 SQL 查詢（`.sqlx/` 是它們的快取，由 `cargo sqlx prepare` 產生，不手改） | 遷移、任意 SQL 腳本 |
| `.sqlx/` | 上述查詢快取 | 手寫內容 |
| `web/` | 管理後台前端（Node 專案） | 後端程式 |
| `scripts/` | 檢查閘門、備份／還原／部署工具與其 `test_*.py` | 閘道業務邏輯 |
| `deploy/` | Docker、compose、systemd 單元與部署用檔案 | 應用程式碼 |
| `docs/` | 一個主題一份文件；`evidence/`、`incidents/`、`superpowers/` 是證據、事故與計畫存檔 | 可執行程式 |
| `loops/` | 每個長任務一份現況檔，索引在 `loops/README.md` | 設計文件 |
| `.github/` | CI 流程 | 其他工具設定 |
| `.repo-policy/` | Git hook 規則與其測試 | 專案程式 |

根目錄的檔案只有設定與入口：`Cargo.toml`、`Cargo.lock`、`build.rs`、`Dockerfile`、`.dockerignore`、`.env.example`、`.gitattributes`、`.gitignore`、`LICENSE`、`README.md`、`AGENTS.md`、`CLAUDE.md`、`DEVELOPMENT.md`、`HANDOFF.md`。實作檔、筆記、輸出檔不放根目錄。

## `src/` 第一層

`src/*.rs` 是平放的單一職責模組（`quota.rs`、`scheduler.rs` 等），規模大了才用同名資料夾拆分。

| 目錄 | 放什麼 | 不放什麼 |
| --- | --- | --- |
| `src/adapters/` | 各 Provider 的協定轉換與串流處理 | 路由、資料庫 |
| `src/admin/` | 後台 API 的業務邏輯 | HTTP 路由定義 |
| `src/orchestration/` | 任務拆解與多模型編排 | Provider 協定細節 |
| `src/routes/` | HTTP 路由：驗參數、呼叫服務、對應錯誤 | 業務邏輯、SQL |
| `src/services/` | 請求流程的業務邏輯（准入、配額、串流） | HTTP 細節、Provider 協定 |

## 新增前的判斷順序

1. 先找既有模組有沒有同職責；有就加進去，不另開。
2. 既有目錄能放就放，新子目錄要有一句話講得清的職責。
3. 新的根目錄或 `src/` 第一層目錄：不必等人核准，但同一次修改要改 `scripts/check.sh` 的 `layout` 白名單和本文件，並在交付時列出，寫明為什麼既有目錄不行。
4. 不開 `misc`、`temp`、`tmp`、`others`、`helpers`、`utils`、`common`；閘門會擋。

## 檔案類型放哪裡

`layout` 也檢查檔案類型的位置（`docs/evidence/` 是存檔，不受限）：

| 類型 | 只能放 |
| --- | --- |
| 腳本（`.py`、`.sh`、`.ps1`、`.bat`） | `scripts/`、`.repo-policy/`、`tests/` |
| `.sql` | `migrations/`、`queries/`、`deploy/`、`scripts/`、`tests/` |
| `docs/` 的檔案 | `.md`、`.pdf`、`.json`、`.png`、`.svg`、`.html` |
| `.log`、`.pid`、`.tmp`、`.out`、`.err`、`.lock`（`Cargo.lock` 除外） | 不進 Git；暫存與輸出放 `.artifacts/` |

資料夾名 `shared`、`helper`、`util` 與上面的 `misc` 等同樣禁用。已存在、尚未整理的違規記在 `scripts/structure-exceptions.txt`（一行一條路徑，後面寫原因），檢查時略過；只能因整理而刪條目，不能為了讓新檔通過而新增。

## 為什麼這樣做

- 只檢查根目錄、`src/` 第一層和禁用名稱：這三處是 AI 最常亂開目錄的地方，且能機械判斷。
- 放棄：不檢查模組依賴方向，也不搬動 `scripts/` 與 `tests/` 的平放檔案；前者此 repo 還沒出現問題，後者搬動成本高於效益。
