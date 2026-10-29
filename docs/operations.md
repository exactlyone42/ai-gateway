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

Each flush sends every currently ready envelope in one PostgreSQL function
call. After the first database failure, the process leaves all envelopes on
disk and makes no automatic retry loop. Restore PostgreSQL, then restart the
gateway once; startup applies migrations and performs one batched recovery
before accepting traffic.

## 部署：新版驗收成功才切換

此流程只支援原本已設定 `SCHEDULER_PASSTHROUGH=true` 的直接轉送模式；此模式不由 Gateway 排隊或計算同時請求上限，因此允許新舊版短暫並行。請求數、token 與金額配額仍由資料庫交易保護。一般排隊模式保留單實例鎖，部署入口會拒絕，也不會自動修改模式。這不是一般多副本支援；不可混合兩種模式，重疊期間的登入與政策快取通知也不會跨實例傳送。

使用 `scripts/deploy.sh --image <映像> --revision <12位commit> --source-container <目前容器> --port <未使用的本機PORT> --evidence <新的證據目錄>`。正式入口保持原網域，候選版只綁本機。建置映像須包含同次建置的 `/app/migrations.sha384`；標準 Dockerfile 已一併產生。本機建置可使用 `deploy/Dockerfile.runtime`，但執行檔、前端與遷移清單必須取自同一 commit；不可沿用未重建的舊執行檔。

部署先逐一比對映像與正式資料庫的遷移內容及版本集合；不符就拒絕，絕不修改資料庫紀錄來通過。此入口刻意拒絕新增遷移；新資料結構須另行完成新舊版相容驗收後才能部署，不能以空資料庫測試取代。CI 同時核對已發布遷移的固定內容。

候選容器沿用目前容器的登入與資料庫設定，獨立保存待寫入記帳資料；不更換舊容器、不共用可被候選版覆寫的設定檔。先驗就緒與版本，再以原 Claude Code 實送。通過後才載入 Caddy 設定，並再次核對公開版本與原 Client 實送；切換前先啟動原 Client 長回答，看到串流後才切換，最後確認同一個請求正常結束，且切換後仍收到文字；中斷後續傳、重送、被掛勾要求重寫，或只有最終成功訊息，都不算通過。驗收用可執行的程式資料範例，避免長篇說明觸發既有輸出長度限制；不關閉掛勾。公開驗收失敗即切回；候選版保留供查驗，不強殺可能尚未結束的請求。

成功紀錄寫入 `accepted.json`，保留舊容器。舊版沒有請求計數時不自動停止；有計數時須連續確認請求數為零且沒有尚未關閉的 HTTP 連線，才停止舊容器；無法確認就保留。`/health` 的 `active_requests` 計算到回應本文釋放，查詢健康狀態本身不增加計數。固定等待時間不是下線證據。

Caddy 載入設定不代表所有長連線都安全：Claude 的 SSE 串流以 `python3 scripts/test_caddy_cutover.py` 在同版隔離 Caddy 驗證跨切換完整收到；此項不宣稱 WebSocket 支援。資料庫主機故障、外部供應商故障不屬部署成功保證。

舊的直接 `compose up --build` 會先取代服務，不可用於正式 Gateway 更新。主機既有 compose 包裝入口須拒絕啟停 gateway；資料庫維護不受影響。

## Backup and restore

加密全文保存的備份與還原尚未完成：現有工具沒有一併保存加密暫存及處理還原後的主金鑰計數。只要目前資料庫已有全文保存結構，工具會在備份或停止服務前拒絕操作；若備份檔本身包含全文保存結構，也會拒絕還原。這是暫時的資料保護，不能把它當成全文保存已有備份。手動使用其他工具還原也尚未安全驗證。

`scripts/capture_backup.py`、`scripts/verify_capture_backup.py` 與 `scripts/restore_capture_backup.py` 是隔離測試用原型，尚未接入正式服務。備份在協調鎖下將資料庫、帳本暫存、加密內容暫存與金鑰目錄分別加密，再以指定的 GPG 金鑰簽署清單；驗證時必須從可信來源指定簽署者識別值。還原只接受空的隔離目標，並建立新金鑰。加密收件人的 GPG 私鑰必須另外安全保存，否則無法還原；簽署金鑰也需要妥善保管。備份副本的刪除政策及所有正式寫入路徑的協調尚未完成，不得當成正式備份方案。參考 [PostgreSQL 備份說明](https://www.postgresql.org/docs/current/app-pgdump.html) 與 [GnuPG 加密說明](https://gnupg.org/documentation/manuals/gnupg/Operational-GPG-Commands.html)。

開發版提供 `agent-pool-gateway --initialize-content-capture`，只在三個保存目錄安全且空、資料庫無保存紀錄及其他用戶連線時建立首把隨機金鑰和獨立計數檔。已有金鑰或紀錄時會拒絕，不會覆蓋；若執行中斷，先檢查目錄，不要自行清空或重試。此指令及全文保存尚未部署，正式上線流程與備份仍須完成驗收。

`./scripts/backup.sh create [directory]` takes the PostgreSQL dump and the
ledger spool volume together, because a database restored without its spool
loses the envelopes that had not reached it yet. `./scripts/backup.sh restore
<directory>` puts both back: it stops the gateway first, so no process holds
connections while the schema is dropped and recreated, and it restores the
spool with the ownership the container needs.

Restore replaces every row in the target database. A restore is only finished
when the service answers again: `/ready` returns `200` and an admin login token
issued before the backup still creates a session. Verified on 2026-09-12 by
deleting both volumes and restoring: the login token returned `401` against the
empty stack and `201` after the restore.

`create` keeps the newest `BACKUP_KEEP` dated directories (default 14) and
removes older ones, so a scheduled run neither fills the disk nor waits for
someone to prune it. Only directories it named are removed; anything else in
the backup root is left alone.

Run it daily from the host scheduler. On the current host that is 05:30
Asia/Taipei, held by a lock file so a slow run cannot overlap the next one,
with output appended to `backup.log`. Verified on 2026-09-12 by running the
scheduled command in a stripped environment like the scheduler's own.

A failed backup is currently visible only in that log. Nothing alerts on it
yet; that belongs with the rest of the alerting work.

## Process shutdown

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
| `backup_stale` / `backup_missing` | Newest backup older than `BACKUP_MAX_AGE_HOURS` (30) | The daily backup stopped, so a restore would lose a day or more | Run `./scripts/backup.sh create` and fix the schedule | yes |
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

## 供應商金鑰可省略

Gateway 的登入驗證仍然保留。未設定 OpenAI API key 時不建立該供應商路徑；Anthropic 依既有 Client 登入轉送、伺服器 Auth Token、API key 的順序選擇，三者都沒有時不建立該路徑。缺少供應商設定不應讓整台 Gateway 無法啟動。

正式環境若明確填入範例金鑰，仍會拒絕啟動；部署不得自動補入 replace-me。
