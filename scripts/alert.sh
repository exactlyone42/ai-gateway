#!/usr/bin/env bash
# The one place an operational failure is reported from.
#
#   ./scripts/alert.sh <severity> <name> <message>
#
# severity: warn | error
#
# Every alert is appended to ALERT_LOG (default ~/agent-pool-gateway-alerts.log)
# and, when ALERT_WEBHOOK_URL is set, posted there as JSON. The log is the part
# that always works: a webhook that is unset, wrong or down must not be the
# reason a failure goes unrecorded.
#
# This script never fails its caller. An alert that takes the process down with
# it turns one problem into two, and the caller has already decided what its own
# exit status means.
#
# Do not pass secrets: the message is written to disk and sent over the network.
set -uo pipefail

log="${ALERT_LOG:-$HOME/agent-pool-gateway-alerts.log}"
severity="${1:-error}"
name="${2:-unnamed}"
message="${3:-}"
host="$(hostname 2>/dev/null || echo unknown)"
at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

case "$severity" in
  warn|error) ;;
  *) message="[severity '$severity' is not warn or error] $message"; severity="error" ;;
esac

printf '%s %s %s %s: %s\n' "$at" "$severity" "$host" "$name" "$message" >> "$log" 2>/dev/null || true

if [ -n "${ALERT_WEBHOOK_URL:-}" ]; then
  payload=$(ALERT_AT="$at" ALERT_SEVERITY="$severity" ALERT_HOST="$host" \
    ALERT_NAME="$name" ALERT_MESSAGE="$message" python3 -c '
import json, os
print(json.dumps({
    "at": os.environ["ALERT_AT"],
    "severity": os.environ["ALERT_SEVERITY"],
    "host": os.environ["ALERT_HOST"],
    "service": "agent-pool-gateway",
    "alert": os.environ["ALERT_NAME"],
    "message": os.environ["ALERT_MESSAGE"],
}))' 2>/dev/null) || payload=""
  if [ -n "$payload" ]; then
    # One attempt, bounded. A retry loop against a paging endpoint during an
    # incident is how a small outage becomes a rate-limited one.
    if ! curl -fsS --max-time 10 -X POST -H 'content-type: application/json' \
      -d "$payload" "$ALERT_WEBHOOK_URL" >/dev/null 2>&1; then
      printf '%s error %s alert_delivery_failed: could not post %s to the webhook\n' \
        "$at" "$host" "$name" >> "$log" 2>/dev/null || true
    fi
  fi
fi

exit 0
