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
# COMPOSE_PROJECT (default: apg) selects the running stack. The restore stops
# the gateway first: a process holding open connections while the schema is
# dropped and recreated sees a database that is neither the old one nor the
# new one.
#
# WARNING: restore replaces every row in the target database.
set -euo pipefail
umask 077

project="${COMPOSE_PROJECT:-apg}"
postgres="${project}-postgres-1"
spool_volume="${project}_ledger-spool"
# Same uid/gid as the image: the gateway must own its spool after a restore.
spool_owner="10001:999"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

fail() { echo "$*" >&2; exit 1; }

case "${1:-}" in
  create|restore|restore-capture)
    if [ -n "${GATEWAY_DEPLOYMENT_LOCK:-}" ]; then
      deployment_lock_path="$GATEWAY_DEPLOYMENT_LOCK"
    elif [ -n "${HOME:-}" ]; then
      deployment_lock_path="$HOME/.local/state/gateway-deployment.lock"
    else
      fail "無法確認部署鎖路徑，未開始操作"
    fi
    [[ "$deployment_lock_path" = /* ]] || fail "部署鎖路徑必須是絕對路徑"
    mkdir -p "$(dirname "$deployment_lock_path")"
    exec {deployment_lock}>>"$deployment_lock_path"
    if flock -n -E 42 "$deployment_lock"; then
      :
    else
      status=$?
      [ "$status" = 42 ] && fail "部署、備份或還原正在執行，未開始操作"
      fail "無法取得部署鎖，未開始操作"
    fi
    ;;
esac

require_container() {
  docker inspect "$1" >/dev/null 2>&1 ||
    fail "container $1 is not there; set COMPOSE_PROJECT to the running stack"
}

capture_schema_present() {
  if [ "$#" -gt 0 ]; then
    legacy_database_exec psql -U gateway -d gateway -Atq -h /var/run/postgresql -p "$legacy_postgres_port" -X < "$script_dir/../queries/capture_backup_schema_present.sql" ||
      fail "cannot check encrypted capture state; backup operation was not started"
  else
    docker exec -i "$postgres" psql -U gateway -d gateway -Atq < "$script_dir/../queries/capture_backup_schema_present.sql" ||
      fail "cannot check encrypted capture state; backup operation was not started"
  fi
}

require_capture_backup_unsupported() {
  local capture_present
  capture_present=$(capture_schema_present)
  [ "$capture_present" = "f" ] || fail "database has encrypted capture state; safe capture backup and restore are not implemented"
}

require_legacy_gateway_volume() {
  local gateway="${project}-gateway-1"
  local mount
  mount=$(docker inspect --format '{{.State.Status}} {{range .Mounts}}{{if and (eq .Destination "/var/lib/agent-pool-gateway/ledger-spool") (eq .Type "volume")}}{{.Name}}{{end}}{{end}}' "$gateway") ||
    fail "無法確認舊版 Gateway 與入帳磁碟卷，未開始備份或還原"
  [ "$mount" = "running $spool_volume" ] ||
    fail "執行中的 Gateway 與預期入帳磁碟卷不一致，未開始備份或還原"
}

require_legacy_database_source() {
  legacy_identity=$(python3 "$script_dir/verify_legacy_backup_source.py" --gateway "${project}-gateway-1" --postgres "$postgres" --volume "$spool_volume") ||
    fail "無法確認舊版備份的資料庫來源，未開始備份或還原"
  read -r legacy_gateway_id legacy_postgres_id legacy_gateway_started legacy_postgres_started legacy_postgres_port extra <<< "$legacy_identity"
  [[ "$legacy_gateway_id" =~ ^[a-f0-9]{64}$ && "$legacy_postgres_id" =~ ^[a-f0-9]{64}$ && "$legacy_gateway_started" =~ ^[0-9TZ:.-]+$ && "$legacy_postgres_started" =~ ^[0-9TZ:.-]+$ && "$legacy_postgres_port" =~ ^[1-9][0-9]{0,4}$ && -z "${extra:-}" ]] && (( legacy_postgres_port <= 65535 )) ||
    fail "來源容器識別不正確，未開始備份或還原"
}

legacy_database_exec() {
  docker exec -i "$legacy_postgres_id" env -u PGHOST -u PGHOSTADDR -u PGPORT -u PGSERVICE -u PGSERVICEFILE -u PGDATABASE -u PGUSER -u PGOPTIONS "$@"
}

confirm_legacy_database_source() {
  local current
  current=$(python3 "$script_dir/verify_legacy_backup_source.py" --gateway "$legacy_gateway_id" --postgres "$legacy_postgres_id" --volume "$spool_volume") ||
    fail "備份來源已改變，操作未完成"
  [ "$current" = "$legacy_identity" ] || fail "備份來源已重啟，操作未完成"
  [ "$(capture_schema_present "$legacy_postgres_id")" = "f" ] || fail "資料庫保存狀態已變更，操作未完成"
}

cleanup_legacy_stage() {
  python3 "$script_dir/stage_legacy_backup.py" --cleanup "$legacy_stage_dir" ||
    printf '%s\n' "暫存目錄未能安全清理，請檢查後處理。" >&2
}

create() {
  local root="${1:-backups}"
  local directory="$root/$(date -u +%Y%m%dT%H%M%SZ)"
  require_container "$postgres"
  local gateway="${CAPTURE_BACKUP_GATEWAY:-}"
  local recipient="${CAPTURE_BACKUP_RECIPIENT:-}"
  local signer="${CAPTURE_BACKUP_SIGNER:-}"
  if [ -n "$gateway" ] || [ -n "$recipient" ] || [ -n "$signer" ]; then
    [ -n "$gateway" ] && [[ "$recipient" =~ ^([[:xdigit:]]{40}|[[:xdigit:]]{64})$ ]] && [[ "$signer" =~ ^([[:xdigit:]]{40}|[[:xdigit:]]{64})$ ]] ||
      fail "請完整設定加密備份的 Gateway 與金鑰指紋，未開始備份"
    mkdir -p "$root"
    python3 "$script_dir/capture_backup.py" --gateway "$gateway" --postgres "$postgres" --destination "$directory" --recipient "$recipient" --signer "$signer" > /dev/null
    python3 "$script_dir/verify_capture_backup.py" "$directory" --signer "$signer" --recipient "$recipient" --write-receipt > /dev/null
    echo "加密備份已建立並驗證：$directory"
    return
  fi
  local capture_present
  capture_present=$(capture_schema_present)
  [ "$capture_present" != "t" ] || fail "database has encrypted capture state; safe capture backup and restore are not implemented without gateway and key fingerprints"
  [ "$capture_present" = "f" ] || fail "無法確認加密保存狀態，未開始備份"
  require_legacy_gateway_volume
  require_legacy_database_source
  confirm_legacy_database_source
  mkdir -p "$root"
  mkdir "$directory" || fail "無法建立備份目錄；若目的地已存在，原資料不會改變"
  chmod 700 "$directory"

  # -Fc keeps the dump restorable with --clean, which is what makes a restore
  # over a live schema work.
  legacy_database_exec pg_dump -U gateway -d gateway -Fc -h /var/run/postgresql -p "$legacy_postgres_port" > "$directory/gateway.dump"
  confirm_legacy_database_source
  docker run --rm -v "$spool_volume":/spool -v "$(cd "$directory" && pwd)":/out \
    alpine:3 tar -czf /out/ledger-spool.tgz -C /spool .
  confirm_legacy_database_source

  # An empty dump restores cleanly and silently loses everything, so refuse to
  # report success for one.
  [ -s "$directory/gateway.dump" ] || fail "the database dump is empty"
  [ -s "$directory/ledger-spool.tgz" ] || fail "入帳備份是空的，備份未完成"
  docker run --rm -v "$(cd "$directory" && pwd)":/backup:ro alpine:3 tar -tzf /backup/ledger-spool.tgz > /dev/null ||
    fail "入帳備份無法讀取，備份未完成"
  confirm_legacy_database_source
  printf 'ok\n' > "$directory/.legacy-complete"

  echo "backup written to $directory"
  ls -l "$directory"
}

restore() {
  local directory="${1:?restore needs the backup directory}"
  local marker="$directory/.legacy-complete"
  [ -f "$marker" ] && [ ! -L "$marker" ] && [ "$(wc -c < "$marker")" -eq 3 ] && [ "$(head -c 4 "$marker")" = "ok" ] ||
    fail "備份缺少完成紀錄，請先檢查並使用安全還原流程"
  [ -s "$directory/gateway.dump" ] || fail "no database dump in $directory"
  [ -f "$directory/ledger-spool.tgz" ] || fail "no spool archive in $directory"
  require_container "$postgres"
  legacy_stage_dir=$(mktemp -d "${TMPDIR:-/tmp}/gateway-legacy-restore.XXXXXXXX") || fail "無法建立還原暫存目錄"
  trap cleanup_legacy_stage EXIT
  python3 "$script_dir/stage_legacy_backup.py" --dump "$directory/gateway.dump" --archive "$directory/ledger-spool.tgz" --destination "$legacy_stage_dir" ||
    fail "備份未通過安全檢查，未開始還原"

  local inventory
  inventory=$(docker exec -i "$postgres" pg_restore --list < "$legacy_stage_dir/gateway.dump") ||
    fail "cannot inspect backup; restore was not started"
  if grep -Eq ' TABLE( DATA)? public capture_[^[:space:]]+ ' <<< "$inventory"; then
    fail "backup contains encrypted captures; safe capture restore is not implemented"
  fi
  docker run --rm -v "$legacy_stage_dir":/backup:ro alpine:3 tar -tzf /backup/ledger-spool.tgz > /dev/null ||
    fail "入帳備份無法讀取，未開始還原"
  require_capture_backup_unsupported
  require_legacy_gateway_volume
  require_legacy_database_source
  confirm_legacy_database_source

  docker stop "$legacy_gateway_id" >/dev/null
  local current_postgres
  current_postgres=$(docker inspect --format '{{.Id}} {{.State.Status}} {{.State.StartedAt}}' "$legacy_postgres_id") ||
    fail "無法確認資料庫狀態，未開始還原；Gateway 已停止，請檢查後恢復"
  [ "$current_postgres" = "$legacy_postgres_id running $legacy_postgres_started" ] || fail "資料庫已改變，未開始還原；Gateway 已停止，請檢查後恢復"
  [ "$(capture_schema_present "$legacy_postgres_id")" = "f" ] || fail "資料庫保存狀態已變更，未開始還原；Gateway 已停止，請檢查後恢復"
  docker run --rm -v "$spool_volume":/spool:ro alpine:3 sh -c 'for p in /spool/.ledger-spool.lock /spool/.capture-backup.lock; do [ ! -L "$p" ] || exit 1; [ ! -e "$p" ] || { [ -f "$p" ] && [ ! -s "$p" ]; } || exit 1; done; entries=$(find /spool -mindepth 1 -maxdepth 1 ! -name .ledger-spool.lock ! -name .capture-backup.lock -print -quit) || exit 1; [ -z "$entries" ]' ||
    fail "入帳目錄仍有資料，未開始還原；Gateway 已停止，請檢查後恢復"
  legacy_database_exec pg_restore -U gateway -d gateway --clean --if-exists --single-transaction -h /var/run/postgresql -p "$legacy_postgres_port" \
    < "$legacy_stage_dir/gateway.dump" || fail "資料庫還原失敗；Gateway 已停止，請先確認資料庫狀態"
  docker run --rm -v "$spool_volume":/spool -v "$legacy_stage_dir":/out:ro \
    alpine:3 sh -c "tar -xzf /out/ledger-spool.tgz -C /spool && chown -R $spool_owner /spool" ||
    fail "入帳還原失敗；資料庫可能已改變且 Gateway 已停止，請先檢查資料"
  docker start "$legacy_gateway_id" >/dev/null

  echo "restored from $directory"
  echo "confirm the service before calling this done: /ready returns 200 and an"
  echo "existing admin login token still creates a session."
}

case "${1:-}" in
  create)  shift; create "$@" ;;
  restore) shift; restore "$@" ;;
  restore-capture) shift; python3 "$script_dir/restore_capture_backup.py" "$@" ;;
  *) printf '%s\n' '用法：backup.sh create [目錄] | backup.sh restore <目錄> | backup.sh restore-capture <參數>' >&2; exit 2 ;;
esac
