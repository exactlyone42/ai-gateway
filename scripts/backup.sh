#!/usr/bin/env bash
# Back up and restore one deployment: the PostgreSQL database and the ledger
# spool volume. Both hold state a redeploy cannot rebuild -- identities,
# credentials, audit history, and usage envelopes that have not reached the
# database yet. docs/operations.md says to back them up together, so this
# takes and restores them as one unit.
#
#   ./scripts/backup.sh create [root]        # default root: ./backups
#   ./scripts/backup.sh restore <directory>
#
# `create` writes one dated directory under the root and then keeps the newest
# BACKUP_KEEP of them (default 14), so a scheduled run does not fill the disk
# and does not need anyone to prune it by hand. Only directories this script
# named are ever removed.
#
# COMPOSE_PROJECT (default: apg) selects the running stack. The restore stops
# the gateway first: a process holding open connections while the schema is
# dropped and recreated sees a database that is neither the old one nor the
# new one.
#
# WARNING: restore replaces every row in the target database.
set -euo pipefail

project="${COMPOSE_PROJECT:-apg}"
postgres="${project}-postgres-1"
spool_volume="${project}_ledger-spool"
# Same uid/gid as the image: the gateway must own its spool after a restore.
spool_owner="10001:999"

fail() { echo "$*" >&2; exit 1; }

require_container() {
  docker inspect "$1" >/dev/null 2>&1 ||
    fail "container $1 is not there; set COMPOSE_PROJECT to the running stack"
}

# Removes every dated directory beyond the newest `keep`. The name pattern is
# the guard: a root that also holds notes or a symlink keeps them.
prune() {
  local root="$1" keep="$2" dated
  dated=$(find "$root" -maxdepth 1 -type d -printf '%f\n' 2>/dev/null |
    grep -E '^[0-9]{8}T[0-9]{6}Z$' | sort)
  [ -n "$dated" ] || return 0
  local total
  total=$(printf '%s\n' "$dated" | wc -l)
  [ "$total" -gt "$keep" ] || return 0
  printf '%s\n' "$dated" | head -n "$((total - keep))" | while IFS= read -r old; do
    rm -rf "${root:?}/${old:?}"
    echo "removed old backup $old"
  done
}

create() {
  local root="${1:-backups}"
  local keep="${BACKUP_KEEP:-14}"
  local directory="$root/$(date -u +%Y%m%dT%H%M%SZ)"
  require_container "$postgres"
  mkdir -p "$directory"
  chmod 700 "$directory"

  # -Fc keeps the dump restorable with --clean, which is what makes a restore
  # over a live schema work.
  docker exec "$postgres" pg_dump -U gateway -d gateway -Fc > "$directory/gateway.dump"
  docker run --rm -v "$spool_volume":/spool -v "$(cd "$directory" && pwd)":/out \
    alpine:3 tar -czf /out/ledger-spool.tgz -C /spool .

  # An empty dump restores cleanly and silently loses everything, so refuse to
  # report success for one.
  [ -s "$directory/gateway.dump" ] || fail "the database dump is empty"

  echo "backup written to $directory"
  ls -l "$directory"
  prune "$root" "$keep"
}

restore() {
  local directory="${1:?restore needs the backup directory}"
  [ -s "$directory/gateway.dump" ] || fail "no database dump in $directory"
  [ -f "$directory/ledger-spool.tgz" ] || fail "no spool archive in $directory"
  require_container "$postgres"

  docker stop "${project}-gateway-1" >/dev/null
  docker exec -i "$postgres" pg_restore -U gateway -d gateway --clean --if-exists \
    < "$directory/gateway.dump"
  docker run --rm -v "$spool_volume":/spool -v "$(cd "$directory" && pwd)":/out \
    alpine:3 sh -c "tar -xzf /out/ledger-spool.tgz -C /spool && chown -R $spool_owner /spool"
  docker start "${project}-gateway-1" >/dev/null

  echo "restored from $directory"
  echo "confirm the service before calling this done: /ready returns 200 and an"
  echo "existing admin login token still creates a session."
}

case "${1:-}" in
  create)  shift; create "$@" ;;
  restore) shift; restore "$@" ;;
  *) sed -n '2,13p' "$0"; exit 2 ;;
esac
