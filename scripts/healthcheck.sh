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
newest=$(find "$backup_root" -maxdepth 2 -name gateway.dump -printf '%T@\n' 2>/dev/null |
  sort -n | tail -1)
if [ -z "$newest" ]; then
  "$alert" error backup_missing "no backup found under $backup_root"
  failed=1
else
  age_hours=$(( ( $(date +%s) - ${newest%.*} ) / 3600 ))
  if [ "$age_hours" -gt "$backup_max_age_hours" ]; then
    "$alert" error backup_stale \
      "the newest backup under $backup_root is ${age_hours}h old, over the ${backup_max_age_hours}h limit"
    failed=1
  fi
fi

# A check that stopped running looks exactly like one that keeps passing: both
# are silent. This stamp is the difference -- an old stamp says the scheduler
# stopped, which nothing else here can tell you.
if [ "$failed" = 0 ]; then
  date -u +%Y-%m-%dT%H:%M:%SZ > "${HEALTHCHECK_STATUS_FILE:-$HOME/agent-pool-gateway-healthcheck.status}" 2>/dev/null || true
  echo "ok: gateway answering, ready, and backed up recently"
fi
exit "$failed"
