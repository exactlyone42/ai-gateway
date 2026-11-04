# Request／Response 全文保存設計

## 目的

試行期預設開啟，保存每一次模型請求與回應，讓後續能比較 Prompt、Skill、模型與 Agent
版本的品質。請求及每段回應必須先寫入加密保存層並確認持久化，再送往下一方；保存失敗不得被當成成功。資料庫暫時不可用但加密暫存仍可持久保存時，串流可以繼續。

## 技術選型

採用「PostgreSQL metadata + 分區 `BYTEA` 全文」：

- PostgreSQL 保存租戶、請求、模型、Agent／Prompt／Skill 版本、雜湊、大小、保存期限、完整性狀態及加密後全文。
- 目前整個資料庫放 SSD。全文表依月份分區；未來有 HDD 時，把過期但仍需保留的舊月份分區搬到 HDD tablespace，程式與查詢介面不變。
- 每個 block 的明文最多 1 MiB，長內容切成有順序的 `BYTEA` block，避免單列過大並支援串流讀寫；PostgreSQL 仍會用 TOAST 處理寬欄位。
- OpenTelemetry 只保存不含內容的計數、延遲與錯誤，不承載 Prompt／Response 全文。
- 不引入 ClickHouse。第一版的需求是依 request、session、版本與時間查找，不是大規模任意分析；等實測查詢量證明 PostgreSQL 不足才另立遷移計畫。

不選 S3／MinIO：目前沒有雲端或 NAS，新增一套物件服務只會增加部署、帳密、備份與故障點。
不選「全文放 OTel」：OpenTelemetry 明確指出模型輸入輸出敏感且可能很大，預設不應擷取；遙測與
內容也需要不同權限。

## 保存範圍

每個 Gateway request 保存四段，才能找出轉譯前後差異：

1. `client_request`：Client 送進 Gateway 的 body。
2. `provider_request`：Gateway 實際送給 Provider 的 body；不得包含 Authorization、API key 等 header。
3. `provider_response`：Provider 回給 Gateway 的 body 或逐段串流資料。
4. `client_response`：Gateway 實際回給 Client 的 body 或逐段串流資料。

非串流內容保存原始 bytes。串流內容保存有順序的 frame：序號、相對時間、方向、bytes；重播時不得
用重新解析後的 JSON 取代原始資料。每段都保存 SHA-256、明文／密文大小及完成狀態。

## 歸因資料

Gateway 已能取得 tenant、user、device、client、agent、credential、session、model、provider、
request id 與 trace id。Client 可再送下列選用 header；值只進 PostgreSQL metadata，不轉送 Provider：

- `x-apg-prompt-id`、`x-apg-prompt-version`
- `x-apg-skill-set-id`：由 Client 對排序後的 `skill@version` 清單計算 SHA-256。
- `x-apg-agent-name`、`x-apg-agent-version`
- `x-apg-client-version`
- `x-apg-task-id`：Agent 的任務編號；同一任務的每個請求帶同一個值。

Gateway 只認請求，不知道「任務何時算完成」（含人工核准、工具執行）。任務完成時間由 Agent 記；Gateway
用 `x-apg-task-id` 把同一任務的請求串起來，並提供每個請求的完成時間（帳本 `completed_at`）。兩邊靠這個編號對得上。

每值限制 128 bytes、僅接受可列印 ASCII；缺少時保存 `NULL`，不得從 Prompt 內容猜測。既有 Client
不送這些 header 時行為不變。

這些 header 只用來歸因，不能影響請求本身：缺少、過長或含不合格字元時，該值一律存 `NULL`，請求照常處理，
不回 4xx，也不排隊失敗。`x-apg-task-id` 漏送只會讓該請求不歸入任何任務。

目前兩種請求入口建立帳本紀錄時，會把合格的歸因值寫入獨立資料表；已進入帳本的格式、政策與額度拒絕也會記錄。此變更尚未部署，也不代表全文保存已接入。

## 資料流

```mermaid
flowchart LR
  subgraph DP[Gateway 資料面]
    C[Client route] -->|先保存原始請求| CAP[Capture session]
    C -->|canonical request| P[Provider adapter]
    P -->|先保存上游請求| CAP
    CAP -->|請求保存成功後送出| U[Provider]
    U -->|response / SSE| P
    P -->|先保存每段原始回應| CAP
    CAP -->|保存成功後送出| C
  end

  subgraph CP[內容保存旁路]
    CAP -->|加密並等待落盤確認| SP[本機加密 spool]
    SP -->|有界批次寫入| HOT[(PostgreSQL SSD 熱分區)]
    HOT -.舊月份維護搬移.-> COLD[(PostgreSQL HDD tablespace)]
  end

  A[Admin API] -->|依租戶查 metadata／串流解密| HOT
  A -->|同一查詢介面| COLD
  DP -.只送 metadata／metric.-> O[OpenTelemetry]
```

實線是請求、回應或內容資料；虛線是不含內容的遙測。Capture 在 Gateway 行程內，
SSD 與 HDD 都由同一個 PostgreSQL cluster 管理；HDD 不存在時只使用 SSD。

## 完整性與故障行為

- 啟用全文保存的請求，必須先把 `client_request`、`provider_request` 與 capture manifest 加密寫入有上限的本機 spool，確認持久化後才送 Provider。
- 每批 Provider 回應與將送給 Client 的資料，均須先加密寫入 spool 並確認持久化，才送 Client；等待隊列不得無界成長。
- 資料庫故障時，只要 spool 仍能持久保存就繼續串流並待恢復後寫入；若 spool 無法保存，停止轉送、將 capture 標成 `incomplete` 並告警，不能默默算完整。
- spool 滿時，新請求在送 Provider 前回穩定的 `503 capture_unavailable`；避免接受後才無聲遺失。
- 批次寫入以 `capture_id + leg + block_sequence` 作冪等鍵；重試先查現有 block 雜湊，不盲目重寫。
- 正常關閉會停止接新 capture、等待既有 frame 落盤，再依既有 shutdown 上限結束；未完成項目下次啟動續傳。

## 安全與保存期限

- 試行期全文保存預設開啟（使用者 2026-10-29 定）；全域 `CONTENT_CAPTURE_ENABLED=false` 或租戶政策不是 `full` 時關閉。體積過大就退回只記摘要。
- 政策的 `constraints.content_capture` 可設 `full` 或 `metadata`；各層取較嚴格的限制，任何一層設 `metadata` 都不保存全文。`constraints.content_retention_days` 為 1–365，各層取較短期限，未設定為 30 天。開發環境可進入全文保存；其他環境選擇 `full` 的請求仍在呼叫供應商前回 `503 capture_unavailable`。此版本尚未部署。
- body 在寫入 spool 或 PostgreSQL 前，以每個 capture 的獨立資料金鑰做 AES-256-GCM 分塊加密；資料金鑰再由版本化主金鑰包裝。主金鑰由安全亂數產生 32 bytes，存在受限檔案而非資料庫；啟動會檢查金鑰與目錄。正式保管及部署流程尚未驗收。
- 全新保存區初始化時，會先在受限金鑰目錄保存初始化意圖，再建立金鑰、計數檔及目前金鑰指向。中斷後，僅在資料庫與暫存仍符合初始化條件、既有檔案與意圖相符時續跑；資料不符就保留原檔並拒絕。意圖尚在時服務不使用該金鑰，備份也會拒絕。建立意圖前若留下未完成暫存檔，目前會拒絕並需人工檢查；若資料庫、計數檔與意圖同時回退，仍無法自動判定金鑰曾否使用。
- 加密使用隨機 96 位元 nonce 時，同一把金鑰的加密次數必須有持久、跨程序可檢查的上限；主金鑰最多包裝 1,048,576 次，每個 capture 每個方向最多 1,000,000 區塊。主金鑰以 SHA-256 指紋共用計數，同一金鑰換名稱不能重設；每次建立保存紀錄前先保留包裝額度，失敗的額度也不回收。上限依 [NIST SP 800-38D](https://nvlpubs.nist.gov/nistpubs/Legacy/SP/nistspecialpublication800-38d.pdf) 保守設定。開發請求流程已使用此保留機制，正式環境未驗收。
- 資料庫從備份還原時，計數可能退回舊值；必須先換用新主金鑰，再恢復新的內容寫入。舊主金鑰只供解密既有內容，不能繼續包裝資料金鑰。啟動及還原防呆尚未實作，不得把備份復原視為已完成驗收。
- 開發請求流程使用獨立保存的金鑰計數檔；若資料庫計數倒退而此檔仍保有較新的數值，會拒絕舊金鑰。計數檔不得與資料庫一同回復舊備份；若兩者一起回退，這項比對無法偵測，仍必須更換主金鑰。正式服務與備份流程尚未驗收。
- 開發版啟動及建立保存作業時會檢查全文暫存、金鑰及計數檔目錄；三者須使用互不重疊的絕對路徑，不能包含符號連結，目標與各層上級目錄須由服務使用者或 root 持有，且不能開放群組或其他人任意改寫。具有黏滯保護的共用暫存目錄，只允許底下項目由服務使用者或 root 持有。容器使用掛載目錄時須確認容器內看到的擁有者符合此規則；無 root 權限容器的使用者編號對應方式尚未實測。路徑檢查與後續開檔之間仍可能遭替換或重新掛載，不能當成完整防護。開啟全文保存時，金鑰及對應計數檔缺失或損壞會使啟動失敗。容器已分配三個獨立磁碟卷，但首次建立金鑰的正式流程與全文保存仍未完成，不得部署。
- 開發版已接入背景整理及四段內容的加密暫存，程式內測試涵蓋非串流、串流及失敗回應；部分原 Client 已在隔離環境實送。正式環境仍拒絕全文請求，完整驗收及部署尚未完成。
- PostgreSQL 只綁內網／localhost；全文表不授權一般帳務查詢角色，Admin 下載仍需另外驗權。
- 不保存認證 header、Provider credential、Gateway key、cookie；Admin 下載內容本身也要寫不可變 audit event。
- 租戶設定保存天數；第一版預設 30 天、允許 1–365 天。到期工作先分批刪除 content blocks，再將索引標成 `deleted`；刪除失敗重試並告警。
- 開發版在手動刪除或到期清理時，會在同一個資料庫交易寫入租戶與紀錄編號的刪除標記。背景匯入在建立紀錄前取得相同的非阻塞交易鎖並檢查標記，因此即使原紀錄列已移除，也不會重新匯入已標記的暫存。鎖忙時操作失敗，下一輪清理再處理；不同紀錄的鎖識別值可能碰撞，造成暫時拒絕。升級會為資料庫仍保有的 `deleting`、`deleted` 紀錄回填標記，無法回填已被實體移除的紀錄。此標記會隨資料庫一起回退，不能替代備份外的獨立刪除紀錄；正式環境尚未部署。
- 背景清理使用獨立的單一處理名額，即使保存請求的處理名額全滿也能嘗試清理。兩者仍共用資料庫連線，因此資料庫忙碌或故障時清理可能延遲。
- 帳本與 audit log 不跟內容一起刪。備份需定義同一保存政策，避免主資料刪了但備份永久保留。
- 待實作：使用者刪除紀錄時，先完成身分與權限確認，鎖住該紀錄，再在備份之外的獨立保存區持久記下租戶、紀錄編號與刪除次序；另一個可信且不隨備份回退的位置，須以不可倒退的方式保存最新次序和該筆紀錄的驗證值。完成上述兩次寫入後才刪資料庫及暫存，全部完成才回報刪除完成。不得寫入提示、回覆、憑證或資料金鑰。每一階段中斷都須可辨識並安全續跑，不得因資料庫交易失敗就丟棄已持久化的刪除意圖。
- 待實作：還原時保持目標隔離，先核對獨立刪除紀錄的來源、連續性及末筆驗證值與可信位置一致，依租戶和紀錄編號重放備份之後的刪除，連暫存一起清除，再確認已刪內容無法讀回且其他租戶與未刪紀錄不受影響，最後才允許啟動服務。紀錄、可信位置或其連續性無法確認時拒絕啟動；不能只驗雜湊鏈，因完整的舊版本也可能被整份回退。若刪除紀錄及可信位置同時回退或同時遺失，系統無法自行判定最新狀態，不能宣稱可安全還原。
- 備份保存時間須與刪除紀錄的可重放期間一致；開發版 Gateway 在開放服務前會分批恢復及清理已過期紀錄，再重掃暫存，遇到被鎖住的紀錄會等待並定期記錄狀態，可收到停止訊號。這會延後整個服務啟動；清理失敗時一般會保留資料並拒絕啟動。例外是不足完整檔頭且未被鎖住的截斷檔：會保留並記警告，因內容無法驗證，無法確認其身分或保存期限。備份副本仍可能含已刪密文及金鑰，重放刪除不等於已從備份副本移除資料，副本如何到期與刪除尚未定案。開發版已有部分接線和隔離測試，完整方案仍未驗收，不開放正式還原。參考 [歐洲資料保護委員會備份刪除研究](https://www.edpb.europa.eu/system/files/2026-02/edpb_cef-report_2025_right-to-erasure_en.pdf)。
- 容量待處理：目前刪除紀錄最多十萬筆，每次新增都重新驗證全部紀錄，達上限後無法再完成刪除。候選方向是將紀錄分段、在備份中記錄可驗證的位置，並只在確認所有可用備份不再需要舊紀錄後才考慮裁剪；還原時必須拒絕位置過舊或來源不符的備份。裁剪、舊格式相容、備份保存期限及中斷復原均未實作或驗證，不得自行刪除既有紀錄。
- 來源檢查進度：開發版新版備份會簽署刪除紀錄位置，隔離還原將該位置寫入資料庫，Gateway 啟動時核對持有的日誌；接錯來源會拒絕。資料庫原本沒有位置時目前只記警告並繼續啟動，這不適用於安全還原。隔離實測將日誌和見證同時回退到備份時的狀態，已刪資料仍會復活；正式還原仍不可開放。
- 中斷判讀進度：日誌校驗已能在不修改檔案的情況下辨識待完成紀錄；位置檢查以唯讀方式開檔並在有待復原紀錄時拒絕。一般啟動會先核對獨立見證，再決定是否完成本機復原；日誌已滿時復原會拒絕追加，保留既有內容。這條流程已在同機隔離環境驗證，跨主機尚未驗收。
- 獨立見證資料庫進度：已建立隔離測試用的 PostgreSQL 結構與條件式更新介面，只保存安裝識別、次序與驗證值。一般見證帳號只能讀取及呼叫更新函式，不能直接新增、刪除、覆寫或清空位置；函式擁有者須是無登入權限且不屬於其他角色的非特權角色。見證帳號若遭盜用，仍可送入假的下一個驗證值使服務拒絕運作。管理者仍能更改或重建位置，必須獨立管理並核對來源，不能把待還原的舊資料自動當作初始位置；見證遺失時無法自行確認最新狀態。Gateway 的刪除、啟動及驗證伺服器身分的加密連線已接入並經同機隔離測試；見證資料庫及其備份仍須與 Gateway 分開管理，不能將同機測試視為正式防回退驗收。
- Gateway 每次讀取或更新見證前，會檢查目前角色與繼承角色的敏感權限，並拒絕可呼叫見證更新函式以外的其他高權限函式；同機隔離測試涵蓋新增直接寫入權限及新增高權限函式。檢查是當下的快照，無法阻止管理者在檢查後改權限；若權限已被擴大，持有見證帳號的人仍可繞過 Gateway 直接更改見證，事後收回權限也無法證明見證未被改過。因此仍須專用資料庫、獨立管理及稽核。Gateway 拒絕仍在備援狀態的見證資料庫，並在更新時要求交易同步提交；已升格但資料落後的備援仍可能被當成主庫。殺死測試容器後可讀回，不能證明斷電或非同步備援切換不會遺失位置。
- 還原中斷保護進度：開發版還原會先建立未完成標記，完成資料、金鑰及位置寫入後，才在資料庫交易中移除標記。新版 Gateway 與全文保存初始化會在資料庫遷移或建立金鑰前檢查標記，存在時拒絕；舊版 Gateway 不具備這項檢查。此流程目前只做隔離驗證，不能取代正式環境、原 Client、獨立防回退及備份副本刪除驗收。
- 加密備份工具目前仍屬隔離原型：備份時會同時取得內容與帳本鎖，帳本啟動復原、請求寫入及補寫資料庫期間也會持鎖。取消背景寫入及整體還原仍有待驗證的情境，不能用此工具做正式備份；正式備份入口仍會拒絕含內容保存資料表的資料庫。備份期間若啟動復原拿不到鎖，服務會拒絕啟動，需待備份結束後重啟。

## SSD 搬到 HDD／NAS

- 一開始只在既有 SSD 預設 tablespace 建立每月全文分區，不預先要求 HDD。
- HDD 就緒後，由操作人員先掛載固定路徑並建立 PostgreSQL tablespace；新舊位置都必須在同一 cluster 可用。
- 只搬已封存、不再寫入的完整月份分區；逐一移動 table 與 index，前後以 `pg_relation_filepath`、筆數、總 bytes 與抽樣 SHA-256 驗證。
- `ALTER TABLE ... SET TABLESPACE` 會取得鎖並實際搬資料，不在流量尖峰執行；先量一個小分區的時間與磁碟空間。
- 若 NAS 只是網路分享，不直接當 PostgreSQL live tablespace；先把它當備份目的地。要放 live data，應讓 PostgreSQL 跑在可靠掛載／NAS 主機上，另做斷線與 fsync 耐久測試。

## 查詢契約

- `GET /admin/v1/request-captures?created_before=&status=&model=&agent_id=&limit=&cursor=`：依
  `(created_at DESC, id DESC)` 游標分頁，`limit` 1–100。
- `GET /admin/v1/request-captures/{id}`：開發版限管理員查詢同租戶的紀錄編號、使用者、狀態、建立及到期時間，以及四個方向各有多少加密區塊；查詢會重新核對登入狀態並先寫稽核紀錄，不回全文或金鑰。區塊數不是完整性驗證，完整性仍須解密核對。
- `GET /admin/v1/request-captures/{id}/content?leg=client_request`：開發版限管理員讀取，方向可選 `client_request`、`provider_request`、`provider_response` 或 `client_response`。Gateway 先記錄讀取嘗試，核對登入、保存期限與內容完整性後分段解密；不發可繞過 Gateway 權限的公開網址。網路中斷、大檔及部署驗收尚未完成。
- 不存在或不屬於目前 tenant 一律 404；已到期回 410；儲存暫時故障回 503 與 `Retry-After`。

- `DELETE /v1/request-captures/{id}`：使用自己的 Gateway 金鑰刪除自己的紀錄；其他使用者或不存在的紀錄回 404。資料庫與暫存檔均清除才回 204，仍有批次或暫存檔待處理回 202，背景程序繼續清除；重複請求可安全執行。此功能尚未部署。

## 衡量與驗收

- 指標：capture 接受／完整／不完整／拒絕數、spool bytes、資料庫批次延遲、各分區 bytes、四段 bytes、到期刪除積壓。
- 主要驗收：OpenAI Responses、Chat Completions、Anthropic Messages 的串流與非串流各一筆，四段雜湊吻合實際 wire bytes；跨協定請求能看見轉譯差異。
- 陰性對照：關閉租戶政策時不得產生 content block；把 spool 容量降到一筆也放不下時，請求必須在呼叫 Provider 前失敗。
- 效能門檻：開啟保存後，1 MiB 非串流與 1000-frame 串流的 p95 Gateway overhead 相對基準增加不超過 5%，且 Provider／Client 慢速測試不增加未界定記憶體。

## 研究依據

- [OpenTelemetry GenAI spans](https://github.com/open-telemetry/semantic-conventions-genai/blob/main/docs/gen-ai/gen-ai-spans.md)：內容敏感且可能很大，預設不擷取，開啟時可截斷或另行上傳。
- [OWASP Logging Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html)：敏感資料需遮蔽／加密，測試儲存失敗，存取需受控並遵守保存期限。
- [Langfuse Data Retention](https://langfuse.com/docs/administration/data-retention)：trace 內容、audit 與評估資料採不同保存生命週期。
- [PostgreSQL TOAST](https://www.postgresql.org/docs/current/storage-toast.html)：寬 `BYTEA` 會自動壓縮或移到表的 TOAST 儲存。
- [PostgreSQL partitioning](https://www.postgresql.org/docs/17/ddl-partitioning.html)：分區可各自指定 tablespace，較少使用的資料可移到較慢媒體。
- [PostgreSQL ALTER TABLE](https://www.postgresql.org/docs/17/sql-altertable.html)：`SET TABLESPACE` 會搬移 table，index 需另行搬移並注意鎖。
