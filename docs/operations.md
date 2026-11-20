# Operations

## Incident records

- [2026-10-21: Claude Code 工作跑滿 15 分鐘卻沒有產物](incidents/2026-10-21-claude-code-timeout.md)

## Environments

Use separate development, staging and production databases, keys, provider
accounts and model aliases. A client key is valid in exactly one environment.

Outside the development bootstrap, every environment must name the tenants that
may use the server-side provider credentials. Set `PROVIDER_ALLOWED_TENANT_IDS`,
or `ADMIN_BOOTSTRAP_TENANT_ID` together with `ADMIN_BOOTSTRAP_TOKEN`. The
gateway fails closed at startup when neither is present.

## Rollout

1. Threat model and provider-contract review.
2. Staging with synthetic accounts and hard spend caps.
3. Shadow policy evaluation without forwarding production content.
4. Pilot team with low budgets and one model class.
5. Canary production replicas, then measured expansion.

## Usage ledger migration

用量帳本的九項唯讀驗收查詢放在 [查詢腳本](usage-ledger-queries.sql)。可在同一個 `psql` 連線載入後，依腳本標示的參數執行；測試也會載入這份腳本，確認九項查詢可執行。

`20260910220000_usage_ledger.sql` is expand-only: it adds ledger tables, indexes,
constraints, append-only triggers, and identity deletion guards. Identity rows
must be revoked or deactivated instead of deleted so delayed ledger envelopes
cannot lose their foreign-key targets.
Deploy the migration before a binary that writes ledger envelopes.

Rollback the binary first and leave the ledger schema in place. Dropping these
tables is not an operational rollback because it destroys billing evidence. A
later corrective migration must be forward-only, preserve existing rows, and
backfill in one set-based SQL statement. Before any destructive replacement,
take and verify a PostgreSQL backup and document the restore command.

Deploy the migration-forward-compatible binary before any later release adds a
migration. It tolerates unknown newer migration versions while still validating
every migration embedded in that binary. This is what lets an older binary
restart after a rollback without deleting schema or migration history.

## Usage ledger spool

Every accepted inference reserves three fixed-size files in `LEDGER_SPOOL_DIR`
before the provider is called. `LEDGER_SPOOL_MAX_BYTES` is a hard logical and
physical ceiling; when it is full, the gateway rejects the new request before
it can incur provider cost. One file preserves the interrupted state and the
the other two are alternating preallocated checkpoint workspaces. The previous
complete checkpoint remains intact while the next is written, so a torn write
cannot erase all observed usage. The default is 64 MiB with 32 KiB per file,
allowing 682 reserved requests.

An envelope contains metadata only and is fsynced before a response is
considered accounted for. The container mounts the directory from the
`ledger-spool` named volume. Do not place it on ephemeral container storage.
This is a single-instance recovery mechanism, not a replicated ledger; back up
the volume together with PostgreSQL before moving hosts.

用量暫存每批最多處理 32 筆；啟動時會處理完待入帳的批次才開放服務。遇到資料衝突時，程式會有界地拆批，將確認衝突的原檔改名為 `.conflict` 保留，其他正常資料繼續入帳。同一配額預留若被多份暫存引用，這些檔案會一起保留供檢查，不依檔名猜測哪一份正確。

`.conflict` 仍占用暫存容量，程式會記錄錯誤；管理者須先保留備份並查明衝突來源，再決定如何處理。容量滿時新請求會被拒絕。若資料庫連線或設定失敗，程式保留尚未入帳的檔案並停止自動寫入；排除原因後重啟。隔離容器已驗證真實 Gateway 產生的衝突檔可備份還原，還原後 Gateway 可保留原檔並繼續記錄正常用量；正式環境與原 Client 仍待驗收。

## 部署：新版驗收成功才切換

此流程只支援原本已設定 `SCHEDULER_PASSTHROUGH=true` 的直接轉送模式；此模式不由 Gateway 排隊或計算同時請求上限，因此允許新舊版短暫並行。請求數、token 與金額配額仍由資料庫交易保護。一般排隊模式保留單實例鎖，部署入口會拒絕，也不會自動修改模式。這不是一般多副本支援；不可混合兩種模式，重疊期間的登入與政策快取通知也不會跨實例傳送。

開發版只在一般排隊模式監看單實例鎖是否遺失；直接轉送模式未取得該鎖，不會因為缺鎖而自行關機，仍會處理原有停止訊號。一般排隊模式失鎖後仍停止接受新工作，並依關機期限結束舊請求；預設期限為30秒，超過期限的串流會中斷。這個資料庫故障情境尚未滿足持續完成舊串流的目標，不得以直接轉送模式的驗收取代。

使用 `scripts/deploy.sh --image <映像> --revision <12位commit> --source-container <目前容器> --port <未使用的本機PORT> --evidence <新的證據目錄>`。正式入口保持原網域，候選版只綁本機。建置映像須包含同次建置的 `/app/migrations.sha384`；標準 Dockerfile 已一併產生。本機建置可使用 `deploy/Dockerfile.runtime`，但執行檔、前端與遷移清單必須取自同一 commit；不可沿用未重建的舊執行檔。

部署先逐一比對映像與正式資料庫的遷移內容及版本集合；不符就拒絕，絕不修改資料庫紀錄來通過。此入口刻意拒絕新增遷移；新資料結構須另行完成新舊版相容驗收後才能部署，不能以空資料庫測試取代。CI 同時核對已發布遷移的固定內容。

候選容器沿用目前容器的登入與資料庫設定，獨立保存待寫入記帳資料；不更換舊容器、不共用可被候選版覆寫的設定檔。先驗就緒與版本，再以原 Claude Code 實送。通過後才載入 Caddy 設定，並再次核對公開版本與原 Client 實送；切換前先啟動原 Client 長回答，看到串流後才切換，最後確認同一個請求正常結束，且切換後仍收到文字；中斷後續傳、重送、被掛勾要求重寫，或只有最終成功訊息，都不算通過。驗收用可執行的程式資料範例，避免長篇說明觸發既有輸出長度限制；不關閉掛勾。公開驗收失敗即切回；候選版保留供查驗，不強殺可能尚未結束的請求。

成功紀錄寫入 `accepted.json`，保留舊容器。舊版沒有請求計數時不自動停止；有計數時須連續確認請求數為零且沒有尚未關閉的 HTTP 連線，才停止舊容器。停止後還要確認容器正常退出，才記錄 `old_stopped=true`；若狀態不明或異常，`false` 不代表容器仍在運行，須自行檢查。`/health` 的 `active_requests` 計算到回應本文釋放，查詢健康狀態本身不增加計數。固定等待時間不是下線證據。

Caddy 載入設定不代表所有長連線都安全：Claude 的 SSE 串流以 `python3 scripts/test_caddy_cutover.py` 在同版隔離 Caddy 驗證跨切換完整收到；此項不宣稱 WebSocket 支援。資料庫主機故障、外部供應商故障不屬部署成功保證。

舊的直接 `compose up --build` 會先取代服務，不可用於正式 Gateway 更新。主機既有 compose 包裝入口須拒絕啟停 gateway；資料庫維護不受影響。

## Backup and restore

加密全文保存的正式備份與還原尚未完成。工作樹中的 `backup.sh create` 在指定 Gateway 容器與加密、簽署金鑰指紋後，可建立加密備份並立即驗證；`backup.sh restore-capture` 接受隔離還原工具的參數，僅允許空白隔離目標。舊的 `restore` 仍拒絕加密資料。監控及備份尚未在正式環境驗收，不能當成正式備份方案。

指定加密備份時，三項設定必須完整；程式依指定的 Gateway 核對實際資料庫與掛載，不以舊版固定資料庫決定備份方式。現行正式 Gateway 尚未具備加密備份所需的全部掛載，不能直接使用此流程。

建立後的驗證會核對清單中的收件金鑰，並確認實際使用該金鑰或其子金鑰解密；這不代表密文沒有其他收件者。單獨執行驗證時，需指定 `--recipient` 才會檢查預期收件金鑰；未指定時保留舊版驗證行為。

產生驗證紀錄時必須指定 `--recipient`，監控也必須設定 `CAPTURE_BACKUP_RECIPIENT`。舊版驗證紀錄沒有記下是否檢查收件金鑰，因此監控會告警；須建立並驗證新備份，或先保留原紀錄後重新完成驗證。監控仍不會重新讀取密文檢查內容。

建立新備份時會忽略 GPG 設定檔中自動加入的收件者；原先依靠該設定取得備份的人不會自動取得新備份。這項設定不驗證既有備份是否含有其他收件者。

隔離還原的參數可用 `./scripts/backup.sh restore-capture --help` 查看；必須明確指定備份、簽署金鑰、隔離資料庫及各個空磁碟卷。還原程式會重新驗簽、解密及檢查目標，不會以建立備份時的驗證結果取代還原前檢查。

工作樹中的健康檢查會確認最新加密備份的簽署時間、檔案大小，以及建立時完整驗證成功的簽章；排程須提供 `CAPTURE_BACKUP_SIGNER` 與能讀到該公鑰的 GPG 金鑰環。檔案大小可發現截斷，但不能發現等長的內容竄改；還原前仍須重新執行完整驗證，並需定期演練還原。舊版第 3 版備份仍可完整驗證和還原，但沒有簽署的大小，健康檢查會告警直到建立新版備份。資料庫狀態無法確認或最新加密備份不完整時，即使有舊版備份也會告警。

健康檢查通過只表示找到符合來源及時間條件、曾驗證過的備份，不能證明資料庫和入帳檔來自同一時間點。備份協調鎖不能阻止未使用該鎖的其他資料庫連線寫入；在來源排他與完整還原驗收完成前，即使顯示通過，也不能當作正式資料可安全還原的證據。

備份不需要停止 Gateway。備份程式取得協調鎖後，新請求會得到 503 並帶 `Retry-After`，用戶端依此重試；取鎖時若有請求進行中，最多等 10 秒（串流也等到收完），逾時才報錯，不會中斷請求。備份程式被強制終止時鎖隨之釋放，隔離演練中請求在 0.2 秒內恢復。資料庫由 `pg_dump` 自己取一致快照。隔離演練用 `--check-client-continuity`、`--check-lock-timeout`、`--check-stream-drain`、`--check-backup-kill` 驗證；超過 10 秒的串流與正式環境尚未驗證。

隔離演練確認：還原後接回最新的同一份刪除紀錄，備份後的刪除會重新套用；接錯來源會拒絕啟動。但若刪除紀錄與見證同時回到舊版，已刪內容仍會復活。必須先完成獨立防回退與備份副本刪除政策，才能評估正式還原。參考 [歐洲資料保護委員會對備份刪除的說明](https://www.edpb.europa.eu/system/files/2026-02/edpb_cef-report_2025_right-to-erasure_en.pdf)。

`scripts/capture_backup.py`、`scripts/verify_capture_backup.py` 與 `scripts/restore_capture_backup.py` 是隔離測試用原型，尚未接入正式服務。新版備份在協調鎖下加密資料並簽署刪除紀錄位置；舊版備份、撤銷或過期的簽署金鑰均會被拒絕。還原只接受空的隔離目標；完成資料、金鑰與位置寫入前，未完成標記會阻止新版 Gateway 啟動。刪除紀錄及見證不在備份包內，必須另外保留最新的原始資料。Python 還原工具透過 `psql` 讀取獨立 SQL 檔並使用具名變數；它無法直接使用 Rust 的 SQLx 巨集。加密收件人的 GPG 私鑰必須另外安全保存，否則無法還原；簽署金鑰也需要妥善保管。備份副本的刪除政策與防回退尚未完成，不得當成正式備份方案。參考 [PostgreSQL 備份說明](https://www.postgresql.org/docs/current/app-pgdump.html) 與 [GnuPG 簽章狀態說明](https://github.com/gpg/gnupg/blob/master/doc/DETAILS)。

隔離備份要求 Gateway 明確設定五個保存路徑，並與容器掛載一致；資料庫網址須明確指定埠號。備份及還原工具會透過資料庫容器內的 `/var/run/postgresql` 本機 socket 操作，並移除會改變連線目標的環境變數；若資料庫改用其他 socket 路徑，這套原型會拒絕，尚不支援。這些檢查避免工具與 Gateway 實際讀取不同的位置。

還原工具會先在隔離容器計算資料庫備份展開後的 SQL 大小，超過上限時在寫入目標之前拒絕；檢查後會再次驗證備份簽章。此限制不涵蓋索引與資料庫日誌等額外磁碟用量，也不能防止同一帳號在檢查後更換檔案；錯誤中斷後的續跑及舊版 Gateway 忽略未完成標記的風險仍需處理。

要重跑隔離演練，可先編譯目前版本，再指定編譯結果：

```sh
SQLX_OFFLINE=true cargo build --bin agent-pool-gateway
python3 scripts/test_capture_backup_gateway.py --gateway-bin target/debug/agent-pool-gateway
```

演練使用本機 Docker、測試用帳號與模擬上游，不連接正式資料。它會輸出使用的程式指紋，並檢查備份、竄改拒絕及還原後讀舊寫新；執行前須有 `python:3.13-slim`、`postgres:17-alpine` 映像及 GPG。指定其他程式檔時，指紋只能識別該檔，不能證明它來自目前原始碼。

完整資料對帳另加 `--check-restored-state <全新證據目錄>`。正向模式需搭配兩協定串流、過期還原或衝突暫存案例，不能用沒有未刪歷史資料的空集合冒通過。它會建立實際額度，逐欄比對11張帳本、額度與密文資料表；送入閘道、送往模型、模型回應、回給Client四段內容須各自與還原前的雜湊相同。另核對舊金鑰保留、新金鑰使用計數、入帳暫存及本機／遠端已確認的刪除進度，還原後新請求也必須只結帳一次。來源仍是測試回應，不是原Client或跨主機驗收。

完整正向演練可組合 `--check-deletion --check-remote-replay --check-protocol-streams --check-database-privileges --check-readonly-backup-role --check-backup-entry`；另以 `--check-expired-restore --check-database-privileges` 驗過期清理，以 `--check-ledger-conflict --check-database-privileges` 驗非空衝突暫存。每次指定不同的全新證據目錄；失敗亦保留加密備份、測試用GPG金鑰及資料讀回，目錄只供操作者存取。Docker物件仍由原演練收尾，只回收本次建立者。

完成入庫的全文暫存會正常清除；驗收須先證明未刪內容的資料庫密文、資料欄位及四方向內容都相同，才允許對應暫存消失。仍待入帳或隔離的入帳原件則須逐檔保留，不能只比筆數。

同一模式加 `--control-corrupt-restored-state` 會故意改還原庫的一格額度；筆數相同也應退出非零。`--check-deletion --check-remote-witness --control-remote-rollback` 加完整資料對帳時，應抓出已刪內容復活而退出非零；沒有加完整資料對帳的舊控制仍會以退出0表示成功重現復活。這兩者是驗收器的反例，不是正式操作方式。

資料庫備份本身不包含叢集角色；演練先由隔離fixture建立角色，再核對還原後的權限與資料，不能推成正式角色已有備份。[PostgreSQL備份範圍](https://www.postgresql.org/docs/17/app-pgdump.html)及[還原說明](https://www.postgresql.org/docs/17/app-pgrestore.html)是工具依據；資料庫快照也不能替代檔案、密鑰與刪除位置的一致性驗收。

加上 `--check-deletion` 可檢查還原後是否保持刪除，再加 `--check-wrong-journal` 可檢查是否拒絕不同來源。`--check-deletion --check-rolled-back-journal` 檢查日誌與見證同時回退；目前此項會失敗，正式還原仍不可開放。

`--check-deletion --check-independent-position` 會在測試程序保存刪除後的位置，將日誌與本機見證回退後，以 Gateway 的位置檢查核對：舊位置可通過，刪除後的位置必須拒絕。此演練沒有獨立見證服務，也沒有接入 Gateway 啟動路徑，不能取代正式防回退。

獨立見證資料庫的結構位於 `deploy/erasure-witness/`，Gateway 已接入讀取及更新，但目前只供隔離測試，尚不能部署使用。啟用全文保存時必須設定 `CONTENT_CAPTURE_REMOTE_WITNESS_URL`，否則啟動會失敗；只有開發測試編譯可明確啟用本機替身。SQL 由管理者以 `psql` 建置與授權；目前只驗證 PostgreSQL 17，建置需要能切換至無登入擁有者的超級使用者。啟用 `RUN_ERASURE_WITNESS_DOCKER=1` 的完整檢查會執行本機容器權限、加密連線及主備測試。Gateway 會拒絕仍在備援狀態的資料庫，但無法辨識資料落後的備援已被升格。主機名稱不同不代表主機、儲存設備或管理權限真正獨立；非同步複寫切換也可能丟失已確認的位置。正式啟用前仍須驗證獨立主機、初始化、權限變更、同步複寫或避免自動切換，以及真實中斷與還原。

隔離演練可用 `scripts/test_capture_backup_gateway.py --gateway-bin <程式路徑> --check-deletion --check-remote-witness`：Gateway 刪除測試資料後，演練還原較舊的資料庫與本機紀錄，確認最新見證會阻止啟動。另加 `--control-remote-rollback` 會在隔離容器中故意回退見證，確認舊資料會再出現；這是反向對照，也說明見證不能與原始資料一起回退。將 `--check-remote-witness` 換成 `--check-remote-replay` 則保留最新紀錄，檢查還原後是否重新執行刪除。演練使用測試帳號和測試內容，不代表跨主機驗收。

另可單獨使用 `--check-expired-restore`，將測試用過期紀錄放進備份，再檢查還原後清理受鎖時不會開服、解鎖後會先清理及更新見證。過期紀錄被鎖住時，整個 Gateway 會等待，每 30 秒記錄狀態；可發送停止訊號，排除原因後重新啟動續做。損壞的暫存或資料庫錯誤也可能阻止啟動，需先保留原資料並查明原因。

啟動期間若資料庫遷移收到停止訊號，Gateway 會以獨立連線嘗試取消同一個資料庫、角色、程序與啟動時間的遷移，再退出；隔離 PostgreSQL 測試涵蓋交易中斷、其他連線不受影響及重新啟動。若資料庫無法建立取消連線或拒絕取消，日誌會記錄取消未確認；資料庫端工作可能繼續，須確認其狀態後再啟動，不能假設 Gateway 退出即代表遷移停止。

開發版提供 `agent-pool-gateway --initialize-content-capture`，在保存、金鑰、計數、刪除紀錄與見證目錄安全，且資料庫無保存紀錄及其他用戶連線時建立金鑰與紀錄。若初始化中斷或出錯，保留原檔並先檢查原因；此指令及全文保存尚未部署。

啟用內容保存時，`DATABASE_MAX_CONNECTIONS` 減去 `ADMIN_DATABASE_MAX_CONNECTIONS` 至少要有 4 條連線；排程器使用直通模式時至少要 3 條。背景清理需要 2 條，一般請求也需要連線；非直通模式的排程器另占用 1 條。連線不足時服務拒絕啟動；這是最低數量，忙碌時仍可能需要更多。

舊版 `./scripts/backup.sh create [directory]` 會一起保存資料庫和入帳暫存。工作樹會核對 Gateway 的入帳路徑、掛載與資料庫連線；舊版指令固定操作 `gateway` 資料庫及帳號，若 Gateway 使用其他資料庫或來源不一致，就會停止。這仍未涵蓋所有手動切換及還原失敗情境，不得將它視為完整的正式備份防護。

工作樹中的備份、還原與 `deploy_gateway.py` 共用部署鎖，忙碌時直接停止；預設鎖檔在執行帳號的 `~/.local/state/gateway-deployment.lock`。若透過 `GATEWAY_DEPLOYMENT_LOCK` 指定其他絕對路徑，所有部署與備份程序必須使用相同路徑和可見的同一個檔案；不同帳號、遠端主機或沒有使用這把鎖的手動切換不受保護。

舊版 `./scripts/backup.sh restore <directory>` 會覆寫目標資料庫及入帳暫存；工作樹要求備份有完成紀錄，先複製到私有暫存區並重封裝允許的入帳檔案，再檢查目標入帳目錄沒有待處理資料。既有沒有完成紀錄的備份會被拒絕，原檔保留待安全驗證。完成紀錄沒有保護檔案內容，備份期間資料庫與入帳檔也不保證同一時間點；暫存空間不足、資料庫與檔案還原不能一起回滾，以及舊版資料庫遷移相容性仍需處理，因此不可用於正式環境。加密備份應使用上方的隔離還原入口。

入帳封存只接受指定格式的一般檔案與根目錄；連結、巢狀目錄、延伸標頭、重複名稱及損壞結尾會被拒絕。這種限制可能拒絕其他工具製作的舊封存，不會修改原檔；還原暫存需要額外磁碟空間，檢查程序記憶體上限為 128 MiB。

備份不會自動刪除。管理者須監控剩餘空間，並在確認保留政策與需要保留的資料後，才處理舊備份。

目前正式 Gateway 使用獨立的候選版入帳磁碟卷及 `gateway_jev_local` 資料庫，與舊版備份的預設卷及 `gateway` 資料庫不同。2026-11-03 核對使用者的 cron 與 systemd 使用者計時器，沒有發現 Gateway 備份排程；其他排程來源尚未完整核對，不能因此認定沒有備份。

工作樹的健康檢查會以 `CAPTURE_BACKUP_GATEWAY` 指定執行中的 Gateway，核對它實際連接的資料庫；未指定時使用專案的預設 Gateway 容器。來源無法確認時告警。舊版未簽署的備份無法由此確認來源和完整性，即使檔案存在也會告警。

加密備份的簽署清單必須記錄目前 Gateway 與 PostgreSQL 容器的識別值，監控才會判為相符。重新建立任一容器後，即使沿用資料，舊備份仍會告警，直到完成新備份及驗證。沒有資料庫容器識別值的舊備份仍可檢查與隔離還原，但不會通過新鮮度監控；此檢查尚不能排除資料或磁碟卷在同一容器中被替換。

新版清單也記錄 PostgreSQL 叢集識別值與資料庫編號，備份前後會檢查兩者是否改變；監控會與當前資料庫比對。資料庫帳號若無權讀取這些資訊，備份和監控會失敗。從相同來源複製或回退資料可能保留識別值，因此這些檢查不能證明資料內容或磁碟卷沒有被替換。

新版清單另保存五個儲存掛載的身分摘要，備份前後及監控時會比對；卷選項不納入摘要。此流程目前只接受 Docker 磁碟卷及 Unix socket 連線，刪除紀錄與見證不接受匿名卷；主機目錄掛載尚無可靠的來源核對，會拒絕備份並告警，後續須補上安全支援。Unix socket 不保證 Docker 與執行備份的程式在相同檔案系統命名空間，Docker Desktop 或轉接的遠端 socket 尚未驗證。卷內資料原地覆寫及回退仍無法靠掛載身分判斷。v3 至 v6 清單仍可驗證，但不會通過新版新鮮度監控；更舊的 v2 尚無安全還原途徑。

Docker 磁碟卷的建立時間可能只有秒的精度；同名卷若在同一時間精度內重建，且其他辨識資料也相同，不能保證被發現。這些資訊只用於來源核對，不代表內容安全。

v2 舊備份可用 `python3 scripts/verify_capture_backup.py <備份目錄> --signer <簽署金鑰指紋> --inspect-legacy-v2` 唯讀檢查簽章、檔案、解密及格式；可另加 `--recipient <收件金鑰指紋>`。v2 沒有刪除位置，檢查通過也不能直接還原，否則可能復活已刪資料；此檢查不建立驗證紀錄。

目前正在建置現行服務的入帳資料加密備份。v8 入帳備份的唯讀格式檢查不產生驗證紀錄，也不能作為還原許可。工作樹有隔離還原程式原型，但尚未經完整實機演練及審查，不可用於正式還原；目前也沒有 v8 備份建立流程。既有加密保存還原入口會拒絕 v8。

工作樹可用 `python3 scripts/ledger_backup_source.py --gateway <Gateway 容器> --postgres <資料庫容器>` 唯讀核對入帳來源；輸出不包含憑證。此檢查沒有建立備份。全文保存設定未明確關閉時預設拒絕；若僅需調查，可加 `--inspect-unconfirmed`，此時 `capture_setting` 為 `unconfirmed` 並以非零狀態結束。即使設定值寫明關閉，也必須確認執行中的版本確實採用該設定，不能僅憑此結果執行入帳備份。

## Process shutdown

工作樹中的 Compose 預設給 Gateway 60 秒結束時間；若調高排空或收尾時間，須依下方公式透過 `GATEWAY_STOP_GRACE_PERIOD` 另行加長。部署程式會依容器的排空與收尾設定計算等待時間，最少 60 秒；新建候選容器及停止舊容器都使用該值。設定只在建立或操作容器時生效，既有正式容器不會自動更新。

`SIGTERM` and Ctrl-C start the same bounded shutdown sequence:

1. Mark the process as draining. New data-plane requests return `503` with
   `Retry-After`; `/ready` returns `503`, while `/health` remains `200` on the
   same listener.
2. Close scheduler admission and let requests that already own a lifecycle
   lease continue, including SSE response bodies.
3. When active requests reach zero, close the listener. If
   `DRAIN_TIMEOUT_MS` elapses first, cancel the process token and hard-close
   connections. Interrupted reservations are finalized as `cancelled` with
   `shutdown_timeout`, provider/request progress and separate estimated or
   partial provider-reported usage. Partial usage never lowers the outstanding
   quota reservation as if it were final.
4. Within `SHUTDOWN_FINALIZE_TIMEOUT_MS`, wait for response-body ownership to
   be released, durable ledger finalizers to finish and the spool worker to
   become idle. Spans and metrics then each get the same bounded flush window.

Set the deployment platform's termination grace period above
`DRAIN_TIMEOUT_MS + 3 * SHUTDOWN_FINALIZE_TIMEOUT_MS + 1 second`, plus an
operational margin. A shorter platform deadline can send `SIGKILL` before the
gateway's own bounds and durability path finish.

`REQUEST_HEADER_MAX_BYTES` is the aggregate application header limit and
returns `413`. The HTTP/1 parser has a fixed buffer at twice that value and
returns `431` before middleware when exceeded. Request bodies retain their
per-protocol limits. Provider DNS/connect/TLS setup is bounded by
`PROVIDER_CONNECT_TIMEOUT_MS`; provider headers and each later stream read are
bounded by `PROVIDER_IDLE_TIMEOUT_MS`. None of these paths retries a provider
request.

送給 Ollama 與 vLLM 的模型請求使用 `LOCAL_PROVIDER_IDLE_TIMEOUT_MS`，預設 300000 毫秒，可設 1 至 600000 毫秒；其他來源的模型請求仍使用 `PROVIDER_IDLE_TIMEOUT_MS`。這個限制包含等待上游回應標頭與每次讀取，不限制整段持續有資料的串流；模型清單同步仍使用原設定。新設定尚未部署。
Streaming clients receive a gateway SSE heartbeat every
`STREAM_HEARTBEAT_INTERVAL_MS` (30 seconds by default), including while the
gateway is still waiting for Anthropic response headers.

## Alerts

`scripts/alert.sh <severity> <name> <message>` is the single exit for an
operational failure. It always appends to `ALERT_LOG`, and posts JSON to
`ALERT_WEBHOOK_URL` when that is set. The log is the part that always works: a
webhook that is unset, wrong or down must not be why a failure goes unrecorded.
It never fails its caller, and it makes one delivery attempt -- a retry loop
against a paging endpoint during an incident turns a small outage into a
rate-limited one.

`scripts/healthcheck.sh` runs from the host scheduler every 15 minutes and
raises the first three rows below. The rest are named here but nothing raises
them yet; each says what is missing.

| Alert | Threshold | What it means | Clearing it | Raised today |
| --- | --- | --- | --- | --- |
| `gateway_down` | `/health` does not answer | The process is gone or the port moved | Start the stack; if it will not start, the startup error names the setting | yes |
| `gateway_not_ready` | `/health` answers, `/ready` does not | The database is unreachable, or the process is draining | Restore the database; a drain clears itself when the deployment finishes | yes |
| `backup_stale` / `backup_missing` | 備份過期、缺失或加密備份未完成驗證；預設上限 30 小時 | 備份可能中斷，無法確認最近的資料可還原 | 確認資料庫、簽署公鑰與備份排程，再檢查最新備份 | yes |
| Provider authentication | Repeated 401/403 from one provider | The provider key is wrong, revoked or out of quota | Rotate the key from its approved source | no: needs a metrics or log reader |
| Spend anomaly | Spend per hour far above the recent baseline | A runaway client, or a model priced differently than assumed | Suspend the credential, then read the ledger for the source | no: needs a baseline nobody has measured yet |
| 5xx rate and p95 latency | Above the service level, once one is agreed | The gateway is failing or slow for everyone | Depends on the cause the traces show | no: needs a metrics backend and an agreed service level |
| Secret rotation overdue | A credential older than the rotation interval | A key has been valid longer than the policy allows | Rotate it; the overlap window keeps the old key working meanwhile | no: needs a rotation interval nobody has set |

A passing check stamps `HEALTHCHECK_STATUS_FILE` with the time. A check that
stopped running looks exactly like one that keeps passing -- both are silent --
and a stale stamp is the only thing here that tells them apart. Verified on
2026-09-12: a failing check leaves the stamp untouched and raises its alert; the
next passing check moves it.

Every alert needs someone who receives it. `ALERT_WEBHOOK_URL` is unset, so
today the alerts reach a file on the host and no person. Naming the owner and
the destination is a decision for whoever operates this, not a default this
file can invent.

## Fail closed

Authentication, model policy, budget reservation and audit durability fail
closed. Optional analytics may fail open. Provider routing fails with a clear
error when no approved deployment is healthy; it never falls back to an
unapproved model or credential source.

## 容器連主機上的 ollama

Gateway 沒有把 ollama 位址寫死在程式裡。位址放在兩個設定值，預設都是 `http://127.0.0.1:11434`：環境變數 `EXTRA_PROVIDER_PROFILES_JSON` 內該項目的 `base_url`，以及資料庫 `providers` 表同一個 `provider_key` 的 `base_url`（用 Admin API 改）。兩處要寫成同一個位址。

問題在網路：Gateway 跑在容器裡，容器裡的 `127.0.0.1` 是容器自己，連不到主機。而主機的 ollama 預設只聽主機的 `127.0.0.1:11434`（2026-11-19 以 `ss -ltn` 確認），所以容器就算改了位址也連不上。要連通需要兩件事：

1. 讓主機 ollama 聽容器能到的位址，例如 `OLLAMA_HOST=0.0.0.0:11434`，或只聽 Docker 網橋位址（目前 `agent-pool-gateway_default` 網橋位址是 `172.22.0.1`，重建網路後可能變，用 `docker network inspect` 重查）。
2. 把上面兩處 `base_url` 改成 `http://172.22.0.1:11434`（或在 `deploy/docker-compose.yml` 為服務加 `extra_hosts: ["host.docker.internal:host-gateway"]` 後用 `http://host.docker.internal:11434`）。

聽 `0.0.0.0` 會讓同網段其他機器也連得到 ollama，沒有金鑰保護；只聽網橋位址則主機上其他程式原本用的 `127.0.0.1:11434` 會連不上。

2026-11-20 正式機實際採用的做法（不動 ollama 本身，仍只聽 `127.0.0.1`）：

1. 使用者級 systemd 服務 `ollama-bridge`（`~/.config/systemd/user/ollama-bridge.service`）用 `socat` 在 `172.22.0.1:11434` 收連線，轉到 `127.0.0.1:11434`。已啟用且使用者帳號開了 linger，重開機後自動起來。
2. 主機防火牆（ufw）只放行 `172.22.0.0/16` 連 `172.22.0.1` 的 `11434/tcp`，其他來源仍被擋。
3. 部署時用 `scripts/deploy_gateway.py --set-env 'EXTRA_PROVIDER_PROFILES_JSON=[…"base_url":"http://172.22.0.1:11434"…]'` 改環境變數；資料庫 `providers` 表 `provider_key='ollama'` 的 `base_url` 要同時改成同一個位址，否則 Gateway 回 503 `provider is not bound to a trusted server-side profile`。
4. 退回：`systemctl --user disable --now ollama-bridge`、`sudo ufw delete allow from 172.22.0.0/16 to 172.22.0.1 port 11434 proto tcp`，並用不帶 `--set-env` 的部署把位址改回 `127.0.0.1`。若 Docker 網路重建、網橋位址改變，要同步改服務、防火牆與兩處 `base_url`。

- Gateway 事故以使用者體感判定：連不上、常出錯、變慢、容易中斷，或任何明顯降低使用品質的情況都要記錄；健康頁亮綠燈也不能排除事故。
- 事故紀錄先用白話交代「原本怎麼運作、哪一步接錯、使用者看到什麼、怎麼處理、如何防止」，讓初階工程師能照著理解與處理；技術名稱與數字只放在後面作查證資料。
- Gateway 搬家或發版後，必須用原本的 Client 真的送出一次請求；只看服務有啟動、健康頁正常或程式編譯成功，不得宣稱完成。
