#!/usr/bin/env bash
# Checks the deployed gateway and reports a failure through scripts/alert.sh.
# Meant for the host scheduler: without it, a gateway that stops answering is
# discovered by whoever tries to use it.
#
#   ./scripts/healthcheck.sh
#
# Checks, in the order a failure matters:
#   1. the process answers at all         -> /health
#   2. it can serve traffic               -> /ready
#   3. the last backup is not too old     -> BACKUP_ROOT
#
# On success it stamps HEALTHCHECK_STATUS_FILE with the time. A stale stamp is
# how anyone finds out the check itself stopped running.
#
# Exits non-zero if anything failed, so a scheduler that reports failures sees
# them too. Repeated alerts are the scheduler's interval, not a retry loop: one
# check, one report.
set -uo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

alert="./scripts/alert.sh"
port="${GATEWAY_PORT:-$(grep -E '^GATEWAY_PORT=' .env 2>/dev/null | cut -d= -f2)}"
port="${port:-8080}"
base="http://127.0.0.1:${port}"
backup_root="${BACKUP_ROOT:-$HOME/agent-pool-gateway-backups}"
backup_max_age_hours="${BACKUP_MAX_AGE_HOURS:-30}"

failed=0

if ! curl -fsS --max-time 5 -o /dev/null "$base/health"; then
  "$alert" error gateway_down "the gateway did not answer /health on $base"
  failed=1
elif ! curl -fsS --max-time 5 -o /dev/null "$base/ready"; then
  # Liveness without readiness is the database or a drain, not a dead process.
  "$alert" error gateway_not_ready "the gateway answers but is not ready on $base"
  failed=1
fi

# A backup schedule that silently stopped looks exactly like one that works,
# until the day it is needed.
project="${COMPOSE_PROJECT:-apg}"
source_state=$(timeout 15s python3 ./scripts/capture_backup_source_state.py --gateway "${CAPTURE_BACKUP_GATEWAY:-${project}-gateway-1}" --postgres "${project}-postgres-1" 2>/dev/null) || source_state="unknown"
read -r capture_schema gateway_id postgres_id database_system_id database_oid storage_id extra <<< "$source_state"
if [[ ! "${gateway_id:-}" =~ ^[a-f0-9]{64}$ || ! "${postgres_id:-}" =~ ^[a-f0-9]{64}$ || ! "${database_system_id:-}" =~ ^-?[0-9]{1,20}$ || ! "${database_oid:-}" =~ ^[0-9]{1,10}$ || -n "${extra:-}" ]]; then
  capture_schema="unknown"
fi
if [ "$capture_schema" = "t" ]; then
  if ! python3 ./scripts/capture_backup_freshness.py "$backup_root" --signer "${CAPTURE_BACKUP_SIGNER:-}" --recipient "${CAPTURE_BACKUP_RECIPIENT:-}" --gateway-id "$gateway_id" --postgres-id "$postgres_id" --database-system-id "$database_system_id" --database-oid "$database_oid" --storage-id "${storage_id:-}" --max-age-hours "$backup_max_age_hours"; then
    "$alert" error backup_missing "加密備份尚未完成、驗證或已過期"
    failed=1
  fi
elif [ "$capture_schema" = "f" ]; then
  "$alert" error backup_missing "舊版備份無法確認來源與完整性"
  failed=1
else
  "$alert" error backup_missing "無法確認備份類型"
  failed=1
fi

# A check that stopped running looks exactly like one that keeps passing: both
# are silent. This stamp is the difference -- an old stamp says the scheduler
# stopped, which nothing else here can tell you.
if [ "$failed" = 0 ]; then
  date -u +%Y-%m-%dT%H:%M:%SZ > "${HEALTHCHECK_STATUS_FILE:-$HOME/agent-pool-gateway-healthcheck.status}" 2>/dev/null || true
  echo "ok: gateway answering, ready, and backup freshness checked"
fi
exit "$failed"
