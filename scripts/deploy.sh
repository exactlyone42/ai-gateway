#!/usr/bin/env bash
# Deploy one build to a host running the compose stack, and put the previous
# one back if anything goes wrong.
#
#   BUILD_REVISION=<id> ./scripts/deploy.sh
#
# The shape follows the deployment this project already does by hand: keep the
# image that is serving, build and start the new one, then prove the new build
# is the one answering. Any failure after the first step restores the kept
# image, because a half-deployed gateway holds provider credentials and serves
# nobody.
#
# COMPOSE_PROJECT (default: apg) selects the stack. The revision is required:
# without it the check after the deployment cannot tell the new build from the
# old one, which is the only thing that makes this more than a restart.
#
# What a rollback can and cannot fix: it restores the previous BUILD, so it
# recovers from a bad release. It does not touch .env, so a broken setting
# breaks the previous image too -- measured 2026-09-12. In that case the script
# says the previous image did not come back and stops, rather than reporting a
# rollback that did not restore the service.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

project="${COMPOSE_PROJECT:-apg}"
compose=(docker compose --env-file .env -f deploy/docker-compose.yml -p "$project")
image="${project}-gateway"
kept="${image}:previous"

revision="${BUILD_REVISION:-}"
if [ -z "$revision" ]; then
  revision="$(git rev-parse --short=12 HEAD 2>/dev/null || true)"
fi
[ -n "$revision" ] || {
  echo "BUILD_REVISION is required: a deployment nobody can verify is a restart" >&2
  exit 2
}

port="$(grep -E '^GATEWAY_PORT=' .env 2>/dev/null | cut -d= -f2)"
port="${port:-8080}"
base="http://127.0.0.1:${port}"

fail() { echo "$*" >&2; exit 1; }

# Reports the revision the running gateway claims, or nothing if it cannot say.
running_revision() {
  curl -fsS --max-time 5 "$base/health" 2>/dev/null |
    python3 -c 'import json,sys; print(json.load(sys.stdin).get("revision",""))' 2>/dev/null || true
}

smoke() {
  local expected="$1" seen
  curl -fsS --max-time 5 -o /dev/null "$base/ready" || return 1
  seen="$(running_revision)"
  [ "$seen" = "$expected" ] || {
    echo "the gateway reports revision '$seen', expected '$expected'" >&2
    return 1
  }
}

rollback() {
  echo "-- restoring the previous image"
  docker image inspect "$kept" >/dev/null 2>&1 ||
    fail "no previous image kept; the stack is left as it is for inspection"
  docker tag "$kept" "${image}:latest"
  "${compose[@]}" up -d --no-build --wait gateway ||
    fail "the previous image did not come back; this needs a person"
  echo "-- previous image is serving revision '$(running_revision)'"
}

echo "-- deploying revision $revision to project $project"

# Step 1: keep what is serving. A first deployment has nothing to keep, which
# is the one case where a failure cannot roll back.
if docker image inspect "${image}:latest" >/dev/null 2>&1; then
  docker tag "${image}:latest" "$kept"
  echo "-- kept the current image as $kept"
else
  echo "-- no image is serving yet: a failure here cannot be rolled back"
fi

# Step 2: build and start. Migrations run inside the process at startup, so
# this single step covers the schema too; a failing migration stops the
# container and never serves.
if ! BUILD_REVISION="$revision" "${compose[@]}" up -d --build --wait; then
  echo "-- the new build did not become healthy"
  rollback
  fail "deployment failed: rolled back"
fi

# Step 3: prove the new build is the one answering.
if ! smoke "$revision"; then
  echo "-- the checks after the deployment failed"
  rollback
  fail "deployment failed its checks: rolled back"
fi

echo "-- deployed: revision $revision is serving and ready"
