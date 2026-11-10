# CLAUDE.md — agent-pool-gateway 專案參數

方法論在全域 `~/.claude/CLAUDE.md` 與 `start-work` skill，這裡**只放本 repo 的參數**，不重打方法。

## 參數

| 項目 | 值 |
| --- | --- |
| 看板 | DnaBoss Project **#2** `Agent Pool Gateway`（`PROJECT_NUMBER=2`、狀態欄位 `Workflow`） |
| loop 檔 | `loops/<slug>.loop.md`，索引 `loops/README.md` |
| 分支 | `feature/<slug>`，off `dev` |
| 整合分支 | `dev`；`main` = 正式（合 `main` 才部署） |
| merge gate | AI 可自行合 `dev`。合 `main` 需 CI 全綠 ＋ 該段落 loop 的完成判準全綠 |
| 迭代檢查 | `./scripts/check.sh gate` |
| PR 前檢查 | `./scripts/check.sh all` |
| 記帳 | `docs/task-ledger.md`，由 `./scripts/token-ledger.sh` 產生 |

## 檔案放哪裡

- 建檔或建資料夾前先看目錄地圖：[docs/directory-structure.md](docs/directory-structure.md)。地圖沒寫的位置不要自己發明，先沿用同類檔案現在的位置。
- 暫存檔、試跑輸出與審查收據放 `.artifacts/`（已被 .gitignore 排除），不放根目錄。
- 新增根目錄項目、模組或改依賴方向時，同一次修改更新那份地圖與 `scripts/check.sh` 的 `layout` 白名單，交付時列出原因。不開 `utils`、`common`、`misc`、`temp` 這類看不出職責的資料夾。
- commit 時 `~/.agents/guards/git_guard.py` 會擋下地圖沒寫的根目錄新增項目，以及上述名稱的新資料夾；`./scripts/check.sh layout` 做同樣的檢查。

## 文件索引（遇到什麼問題才讀，不要一開始全載）

「產出段落」欄標 `—` 的是現在就有；標段落的還不存在，**不要引用它**。

| 文件 | 什麼時候讀 | 產出段落 |
| --- | --- | --- |
| `docs/directory-structure.md` | 建檔、建資料夾或搬檔之前；`./scripts/check.sh layout` 會擋未登記的頂層目錄 | — |
| `docs/work-breakdown.md` | 要知道整體切成哪些段落、依賴順序、現在該做哪一段 | — |
| `docs/engineering-standards.md` | 要動程式碼、寫測試、加指標、開 PR 之前 | — |
| `docs/feasibility.md` | 有人問「這件事到底做不做得到」、或要新增一個 Client | — |
| `docs/architecture.md` | 要知道三個 Plane 的邊界、請求路徑、故障模型 | — |
| `docs/client-compatibility.md` | 要接一個 Client，或有人說「我的工具連不上」 | — |
| `docs/security-and-compliance.md` | 要動憑證存放、訂閱席次邊界、稽核 | — |
| `docs/product-requirements.md` | 要確認角色與驗收條件 | — |
| `docs/operations.md` | 要部署、上正式、處理事故 | — |
| `docs/task-ledger.md` | 要看每段落花多少時間／token、當初的 trade-off | — |
| `docs/loadtest-journal.md` | 要引用效能數字，或要加一輪壓測 | W18 |
| `docs/provider-protocols.md` | 要接新的 Provider，或處理協定轉換落差 | W05／W06 |
| `docs/identity.md` | 要動身分／憑證／Principal 解析 | — |
| `docs/request-policy.md` | 要動政策合併、workload 分類、配額 | W11／W12 |
| `docs/scheduler.md` | 要動排隊、優先權、保留容量、併發上限 | W13 |
| `docs/observability.md` | 要加 metric／trace／log，或查「為什麼看不到」 | —（W07 已交付；後續缺口看 `loops/observability-otel.loop.md` 移交） |
| `docs/admin.md` | 要動後台頁面或 Admin API | W08／W09 |

## 本 repo 的硬約束

1. **核心閘道自己實作，LiteLLM 不是執行期依賴。** 可以讀它的架構學 edge case，不進 `Cargo.toml`。
2. **只集中官方 API 容量，不碰訂閱席次。** 不抽 session token、不共用席次、不自動化互動式登入、不改 Client 執行檔、不做 TLS 中間人。
3. **預設不保存 Prompt／Response 內容**，只留 metadata。要開內容記錄必須是明確 opt-in。
4. **Provider 憑證只存在伺服器端**，任何情況都不回給 Client。
