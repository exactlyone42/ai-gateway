# 工程規範

**這份文件的每一條，要嘛有機器檢查，要嘛寫明驗收動作。** 沒有兩者之一的條目不該寫在這裡——
那是感想，不是規範。

方法論（怎麼工作、怎麼講話、怎麼舉證）在全域 `~/.claude/CLAUDE.md` 與 `start-work` skill。
這份只寫**本專案的工程細則**。

---

## 0. 檢查關卡

```bash
./scripts/check.sh gate    # 迭代中跑這個（快）
./scripts/check.sh all     # 開 PR 前跑這個（＝ CI 跑的全套）
```

| 指令 | 內容 | 什麼時候跑 |
| --- | --- | --- |
| `fmt` | `cargo fmt --all` | 想直接改格式時 |
| `fmt-check` | `cargo fmt --all --check` | 每次 |
| `gate` | fmt-check ＋ clippy ＋ loop 檔紀律 ＋ Rust 禁用構件 | **迭代中只跑這個** |
| `loops` | loop 檔八段落、主體 200 行、必須進索引 | 改 loop 檔後 |
| `bans` | 生產路徑禁用構件、`mod.rs`、硬編模型名 | 每次 |
| `all` / `pre-push` | 上面全部 ＋ `clippy --all-targets` ＋ `cargo test` | **PR 前、合併前** |

**不要每改一行就跑 `all`。** 那是浪費時間，不是嚴謹。

---

## 1. Rust 慣例

### 模組佈局：不用 `mod.rs`

```text
crates/gateway-core/src/
  policy.rs           # 模組根
  policy/
    merge.rs          # 子模組
```

`crates/**` 底下**禁止** `mod.rs`。機器檢查：`./scripts/check.sh bans`。

### 錯誤處理分層

| 層 | 用什麼 |
| --- | --- |
| 邊界（HTTP／CLI／服務進入點） | `anyhow::Result<T>` ＋ `.context("...")` 補上下文 |
| 領域 crate | `thiserror` 具名錯誤 enum，**不對外露 `anyhow::Error`** |
| 跨 crate 共用 | 只共用**協定層**的穩定型別（`ErrorCode`／`ErrorResponse`） |
| HTTP 出口 | **單一** `ApiError`，實作 `IntoResponse`，映射成穩定的 status ＋ error code |

HTTP 映射固定：參數／輸入錯誤 → 400；認證 → 401／403；找不到 → 404；
依賴掛掉或逾時 → 503；預期外 → 500。

### 生產路徑禁用構件

非測試程式碼**禁止**：`unwrap()`、`expect()`、`panic!()`、`todo!()`、`unimplemented!()`、
裸 `assert!`。錯誤要往上傳，不是就地炸掉。

機器檢查：`./scripts/check.sh bans`。
**陰性對照**：在任一非測試檔加一行 `let _ = None::<u8>.unwrap();`，`bans` 必須變紅。

### 依賴管理

版本集中在根 `Cargo.toml` 的 `[workspace.dependencies]`，各 crate 寫
`dep = { workspace = true }`。**不在子 crate 各自釘版本。**

### 併發

- I/O 用 async／await；CPU 密集或同步 I/O **不得**在 async 情境裡阻塞 runtime。
- 通道**一律有上限**（背壓）。無上限通道等於把 OOM 延後發生。
- **禁止** `request → tokio::spawn() → 無上限上游請求`。所有上游併發都要過閘門。

### 模型名稱不進程式碼

`claude-*`／`gpt-*`／`gemini-*`／`grok-*` 這類字串只允許出現在
migration、設定檔、測試 fixture、文件。非測試程式碼命中即紅。
理由：模型名稱會變，換模型必須是改設定不是改程式（規格 §20）。

機器檢查：`./scripts/check.sh bans`。

---

## 2. 錯誤與日誌

- **所有錯誤都要被記錄，禁止靜默吞掉。** 4xx 至少 `warn`，5xx 必 `error`（含請求 ID）。
- 錯誤走**統一出口**，不各處自己 `println!`／`eprintln!` 散寫。
- 日誌用**結構化欄位**，不要把資料塞進字串：
  `error!(provider = %p, model = %m, "upstream rejected")`，不是 `error!("upstream {p}/{m} ...")`。
- 每筆日誌至少帶：`timestamp`／`level`／`trace_id`／`request_id`／`attempt_id`／
  `principal`／`event`／`status`／`provider`／`model`。

### 絕對不記

Provider API secret、閘道金鑰（明文或雜湊）、密碼、access token、
完整 request／response body、PII（姓名／email／電話）。

驗收動作：跑一次 W18 的故障注入場景，把 log 輸出過一遍遮蔽檢查。
**陰性對照**：在日誌欄位塞一個假 token 字串，遮蔽檢查必須變紅。

---

## 3. 測試紀律

### 什麼時候寫測試

**任務的核心目標未經實測驗證前，不寫測試、不磨覆蓋率。**

順序鐵律：**實作核心 → 實測驗證方向 → 方向確認後、合 PR 前才補測試**。

理由：測試是「交付／合 PR」的品質關卡，**不是**「方向對不對」的驗證手段。
方向錯了要整段重寫，先磨的測試全白做。

### 測試分類

| 類型 | 位置 | 驗什麼 |
| --- | --- | --- |
| 單元 | crate 內 `#[cfg(test)]` | 政策合併、配額、憑證、排程、路由、token 正規化 |
| Contract | `tests/contract/` | 協定 ↔ Canonical ↔ Provider 請求的逐欄位對照 |
| 串流 Contract | `tests/contract/` | Provider SSE fixture → Canonical 事件 → Client SSE，**逐事件**比對 |
| 整合 | `tests/integration/` | 用 mock provider 測串流／取消／重試／逾時／429／5xx／格式錯誤／部分回應／工具呼叫 |
| 負載 | `tests/load/` | 併發、排程公平性、故障注入、效能 overhead |

### 舉證責任（最重要的一條）

**「燈是綠的」不構成證據。** 宣稱修好了或驗過了，必須同時附：

1. **陰性對照** —— 注入一個已知的壞情境，該檢查**必須變紅**。不會紅的綠燈是橡皮圖章。
2. **覆蓋率確認** —— 「N passed」要對照「幾條 skipped／根本沒執行」。
   「有在跑」不等於「有在驗」。

**覆蓋率本身不構成證據**：只呼叫函式而不驗行為的測試也能拿到高覆蓋率。
宣稱「測試寫好了」要能說明**測試會因為程式碼被改壞而變紅**。

每份 loop 檔的「完成判準」都必須含至少一條陰性對照。機器檢查抓不到內容品質，
但 `./scripts/check.sh loops` 會強制「完成判準」段落存在，內容由 review 把關。

### 歸因宣稱

說「根因是 X」要有**直接證據**（死在哪一步的 log）。**時間相關性不是因果。**

---

## 4. 可觀測性契約

### Metric 命名與基數

規格 §57 的 13 個 metric 名稱是契約，不要自創同義詞。

**Label 白名單制**。允許當 metric label 的只有低基數維度：
`workload_class`／`urgency`／`quality`／`budget_class`／`provider`／`model`／
`status`／`error_type`／`attempt_number`。

**禁止當 metric label**：`request_id`／`trace_id`／`session_id`／`attempt_id`／
`credential_id`／`user_id`／`device_id`／`agent_id`／任何自由文字。
這些只進 trace 與 log。

理由：高基數 label 會把時序資料庫打爆，而且爆的時候是可觀測性先死，不是被監控的系統先死。

驗收動作：`./scripts/check.sh bans` 檢查 metric 註冊點沒有用到黑名單欄位名。

### 只認 OTLP

閘道程式碼**不得**出現 Grafana／Prometheus／Tempo／SigNoz／Datadog 的專屬 API。
這些名字只允許出現在 `deploy/` 與文件。換後端不該動閘道一行程式碼。

機器檢查：`./scripts/check.sh bans`。

### PostgreSQL 是帳本，Prometheus 不是

精確用量與金額查詢一律走 PostgreSQL。**不要用時序資料庫做帳務**——
它會降採樣、會過期、會聚合，這三件事都讓帳算不準。

---

## 5. 壓測與效能紀律

- **先量到數字證明有效，再談測試品質。** 壓測的本業是量吞吐，不是寫單元測試。
- 每個數字都要記：量測方法、環境、日期、版本。留「大概」等於沒量。
- 紀錄格式固定進 `docs/loadtest-journal.md`：一次一段，含
  「想驗什麼 → 怎麼跑 → 數字 → 結論 → 這個數字證明不了什麼」。
- 效能宣稱要有**前後對照**，不是只有「後」。
- 改動熱路徑（串流轉送、排程決策）前後都要量。

---

## 6. 文件維護義務

- **改了行為就同步改文件。** 不要讓文件跟程式碼說不同的話。
- 決策要留「**為什麼**」與「**放棄了什麼**」，不是只留結果。跨段落的決策記進
  `docs/task-ledger.md`，段落內的記進該 loop 檔的 `## Trade-off`。
- 模組知識**一模組一份文件**，`CLAUDE.md` 放索引表，並標明**「遇到什麼問題才需要讀這份」**——
  讓 AI 啟動時不必全載。
- 每份模組文件附**檔案地圖**（這個模組會動到哪些檔案），改該模組時只載入那些檔案。

### 不能當證據的東西

| 可以當證據 | 不能當證據（是「主張」，引用前要先驗） |
| --- | --- |
| 實際會執行到的程式碼路徑（`檔案:行號`） | 程式碼註解 |
| 指令／測試／查詢的**輸出** | 文件、README、`CLAUDE.md` 自己 |
| 實測結果 | commit message、PR 描述 |
| 規範／官方文件**原文**（附章節或 URL） | loop 檔、看板卡 |
| 型別、schema、DB constraint | 其他 Agent／模型的報告 |

右欄描述的是**意圖**，不是行為。兩者會分岔，而且分岔時沒有徵兆。
要引用右欄，先去驗它講的是不是真的，然後引用「驗證的輸出」。

**講證據要同時講邊界**：「這個檢查證明 X，但證明不了 Y」。說不出邊界代表還沒想清楚。

---

## 7. Git、PR、看板

| 項目 | 規則 |
| --- | --- |
| 分支 | `feature/<slug>`，off `dev`。`<slug>` 與卡、loop 檔同名 |
| 開分支後 | **立刻 `git push -u` 並開 draft PR**（base=`dev`，標題掛卡號） |
| Commit | Conventional Commits（`feat:`／`fix:`／`docs:`／`refactor:`／`chore:`）。每個 commit 獨立可編譯 |
| 行為保持的重構 | 與功能變更**分開提交** |
| 合 `dev` | AI 可自行合，需 `check.sh all` 全綠 |
| 合 `main` | ＝部署正式。需 CI 全綠 ＋ 該段落 loop 的完成判準全綠 |
| 看板 | DnaBoss Project #2，狀態欄位叫 `Workflow`（不是 `Status`），由 `scripts/board-sync.sh` 自動只往前推進 |

**為什麼開分支就要推**：分支只在本機時，其他 session 查 `gh pr list` 看到的是「這張卡沒人做」，
整張卡會被重做一次。查重複靠的是**別人推上去過**。

---

## 8. Loop 檔紀律

見 `loops/README.md`。機器檢查：`./scripts/check.sh loops`（八段落、主體 200 行、必須進索引）。

三條容易犯的：

1. **「已做」只寫已完成的**，不寫打算做什麼（那是「下一步」）。
2. **主體破 200 行 ＝ 任務太大**，先拆，不要把上限往上調。
3. **每完成一段落就回寫**，不要收工才補——補的內容會漏掉當時排除掉的路，
   下一個人（或下一個 session）就會重走一遍。

---

## 9. 安全

- SQL **一律參數化**，禁止字串拼接。
- 金鑰只存雜湊 ＋ 前綴 ＋ 指紋。明文只在建立當下回一次。比對走**常數時間**。
- 不 commit 憑證、`.env`、金鑰。`.env` 必須 gitignored。
- Docker port 預設綁 `127.0.0.1`，不暴露到 LAN。
- Request body 與 header 都要有大小上限；所有上游呼叫都要有逾時。
- CORS 從嚴。**Admin API 與推論 API 的權限完全分離。**
- RBAC 在 **API 層**強制，不是靠 UI 藏按鈕。
- **Provider secret 任何情況都不回給 Client。**
- 正式環境的 secret 走 Secret Provider 介面（未來接 Vault／KMS），第一版不自己造 secret manager。

---

## 10. 資料庫

### 鐵則：非批次行為禁止用迴圈

**「一個動作要對 DB 或遠端服務做 N 次」時，唯一合法解法是一次批次，不是迴圈做 N 次。**
不分讀寫、不分連線／探測／健康檢查，全部都算。

- 禁止 `for`／`while`／`xargs -P`／遞迴 包著 `psql`／DB client／`nc`／`curl` 打同一個端點。
- **連「連不上就重試」的迴圈也禁**。一次失敗就停、回報。反覆連線會觸發對端
  fail2ban／rate-limit 把來源 IP 鎖掉，之後全逾時，反而更慢。
- 對 N 筆做事用集合運算一次做完：`unnest(...)` 一次寫、`ANY(...)` 一次刪、`IN (...)` 一次查、
  `COPY (SELECT …) TO STDOUT` 一次匯出。
- 輪詢外部狀態改用排程心跳，**一次心跳只打一發**；等不到就停手回報。
- 多個查詢合併成一次連線、一個 script，不要一句一個 `psql`。

> 看到自己要寫 `for` 去打 DB 或服務，先停下來——那幾乎一定是錯的，改成一次批次。

### 其它

- 探查前先想「這會掃多少列」。要掃大表先確認必要性，跑完講清楚代價。
- 重查詢（掃全表／大範圍聚合）一次就好，能用既有結果就別重算。
- migration 綁在啟動流程，不讓部署 SOP 多一步。

---

## 11. 長時作業

- **長時間執行的功能必須可觀測**：禁止「按下去 → 黑箱 → 只有成功或失敗」。
  要查得到「做到哪、剩多少、卡在哪」。
- 單一請求撐不完（超過數十秒）的一律改背景工作。
- 所有自動化任務要**可追蹤、可重試、可停用**，並有重複執行防護。
- 單筆失敗要隔離，不能拖垮整批。
- **不自動改寫使用者的原始資料。** 這類動作只給建議，人工核准才執行。

---

## 12. Token 紀律

規則與實測數字見 `docs/task-ledger.md`。三條最重要的：

1. **合併獨立的工具呼叫**。同一回合平行發出只重讀一次 context，拆成多回合就重讀多次。
   實測 92% 的 token 花在重讀 context，不是產出。
2. **子 Agent 用在「廣搜、只要結論」**。判斷標準：要讀的原始資料量 > 起手成本（實測一個
   子 Agent 起手約 135k～216k）才划算。
3. **不要重讀剛改過的檔案**。編輯失敗會直接報錯，不必「確認」。

---

## 13. 明確不做（第一版）

Kubernetes、Kafka、Redis Cluster、ClickHouse、Service Mesh、SSO／企業 IAM、
計費 SaaS、AI Router、Prompt marketplace、自己做可觀測性資料庫、精緻後台 UI、
LiteLLM 當執行期依賴。

**除非實際量測證明需要。要加就先給數字。**

不要過度工程：

- **選最簡單的正確實作**，不確定時先做能用的版本。
- **不為假想的未來需求寫程式碼。**
- **三行重複優於過早抽象** —— 出現第三次才抽。
- 不需要的就刪掉，不留 `_unused`、不寫 `// removed` 註解。
- Monolith first：沒有明確需要就不拆服務、不引 message queue。

---

## 14. 交付說明義務

每次交付要交代：

- 哪些檢查**跑了**、哪些**沒跑**
- 有沒有動到正式環境
- 影響範圍（公開介面／後端行為／資料庫結構／部署設定）
- 還剩什麼風險需要另外處理

**「沒跑的檢查」要主動講，不要等人問。**
