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
| `HANDOFF.md` | 接手工作、要知道哪個工作樹有什麼成果與第一步 | — |
| `docs/engineering-standards.md` | 要動程式碼、寫測試、加指標、開 PR 之前 | — |
| `docs/src-architecture-refactor-plan.md` | 要執行或驗收 src 分層、SQL 檔案化與註解整理 | — |
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

## 程式分層與檔案組織

- 每項實作前先上網查官方文件與適用的最佳實務，確認版本及適用條件，依專案需求選擇並在工作紀錄保留來源與取捨；查無適用資料或來源衝突時明說。
- 保持程式簡潔，依 SOLID 原則分配明確職責：相關行為集中、模組之間只透過必要介面互動；介面應能替換而不破壞呼叫者，避免為套用原則而新增無用抽象或依賴。
- 正式請求依序由中介層處理跨端點門檻、路由層驗證輸入並呼叫服務、服務層執行業務規則；路由層只轉換 HTTP 輸入、輸出與錯誤，不直接執行 SQL、呼叫上游或決定業務政策。
- 資料存取集中在資料層或既有領域資料模組，負責查詢、寫入及交易；服務層負責協調需要一起成功或失敗的操作，交易邊界須完整保留。服務層不依賴路由或 HTTP 回應型別。
- SQL 語句獨立放在 `.sql` 檔；固定查詢使用 SQLx 的 `query_file!`、`query_file_as!` 或 `query_file_scalar!`，並維護編譯時所需的資料庫結構或離線查詢資料。測試用 SQL 也遵守此規則。動態查詢若無法使用這些巨集，須保留參數化，說明原因並避免把 SQL 文字散落在路由或業務邏輯中。
- 依領域與職責拆模組，對外只公開必要介面；檔案難以理解或混合多項職責時拆分，不用固定行數硬切，也不為每一層預先建立沒有用途的抽象。
- 小型單元測試可放在被測模組；大量測試依功能拆至獨立測試模組，跨模組行為放在 `tests/`。測試可用 SQL 建資料及驗證結果，但要與正式路由程式分開；需要測試私有介面時使用模組內的獨立測試檔，不為搬測試擴大公開介面。
- 修改時檢查本次涉及的呼叫路徑與交易邊界，維持既有 API、權限及錯誤行為；既有違規依任務範圍逐步整理，不順手重構無關功能。詳細檢查依 `docs/engineering-standards.md` 的分層章節。
