# AGENTS.md — agent-pool-gateway 專案參數

方法論在全域 `~/.Codex/AGENTS.md` 與 `start-work` skill，這裡**只放本 repo 的參數**，不重打方法。

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

## 文件索引（遇到什麼問題才讀，不要一開始全載）

「產出段落」欄標 `—` 的是現在就有；標段落的還不存在，**不要引用它**。

| 文件 | 什麼時候讀 | 產出段落 |
| --- | --- | --- |
| `docs/work-breakdown.md` | 要知道整體切成哪些段落、依賴順序、現在該做哪一段 | — |
| `docs/engineering-standards.md` | 要動程式碼、寫測試、加指標、開 PR 之前 | — |
| `docs/feasibility.md` | 有人問「這件事到底做不做得到」、或要新增一個 Client | — |
| `docs/architecture.md` | 要知道三個 Plane 的邊界、請求路徑、故障模型 | — |
| `docs/client-compatibility.md` | 要接一個 Client，或有人說「我的工具連不上」 | — |
| `docs/terminology.md` | 要寫文件、訊息或畫面，用到「金鑰」「API 金鑰」「權杖」時 | — |
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
2. **每個人只用自己的帳號，訂閱或 API 金鑰由各人自己選；Gateway 只提供路徑與方法，不提供帳號或額度**（使用者 2026-10-29 定；目前 Gateway 跑在使用者自己的電腦上）。訂閱：權杖由 Gateway 容器持有（`ANTHROPIC_SUBSCRIPTION_TOKEN_FILE`，使用者 2026-10-29 定），管理者用 `PUT /admin/v1/provider-tokens/anthropic` 更新，Client 只帶 Gateway 金鑰；權杖不寫入資料庫與日誌、不回給任何 Client。API 金鑰與本機模型是另外兩條路，都不強制。三個詞（Gateway 金鑰、API 金鑰、權杖）的定義見 `docs/terminology.md`。不替他人登入、不共用席次、不輪替帳號、不自動化互動式登入、不改 Client 執行檔、不做 TLS 中間人。
3. **Prompt／Response 內容只存進專用的加密保存層，不進一般日誌、追蹤或指標**（使用者 2026-10-29 改）。保存層是個人工作紀錄的唯一主紀錄，須加密、分權限、訂保存期限、可刪除，並先寫後送。全記是試行，體積過大就退回只記摘要。尚未實作前，程式仍只留 metadata。設計見 `docs/request-response-storage.md`。
4. **Provider 憑證只存在伺服器端**，任何情況都不回給 Client。
