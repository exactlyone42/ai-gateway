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

## Deployment

`BUILD_REVISION=<id> ./scripts/deploy.sh` keeps the image that is serving,
builds and starts the new one, then confirms the running gateway reports that
revision on `/health`. Any failure restores the kept image. Migrations run
inside the process at startup, so a failing migration stops the container and
never serves.

A rollback restores the previous build, not the previous settings: a broken
value in `.env` breaks the previous image as well. The script then says the
previous image did not come back and stops, because a rollback that did not
restore the service must not be reported as one.

Verified on 2026-09-12: a build whose `/health` was deliberately broken passed
the container health check, which polls `/ready`, and was caught by the check
after the deployment instead. The previous revision was serving again when the
script returned.

## Backup and restore

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

正式環境若明確填入範例金鑰，仍會拒絕啟動；部署不得自動補入 replace-me。檢查部署結果時，除了 ready 與版本，也要用原 Client 真正完成請求，並核對原資料庫、Client 數與登入設定沒有被替換。
