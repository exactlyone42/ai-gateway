import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import select
import subprocess
import sys
import time

from capture_backup import BackupError, database_target, finish_backup_source, inspect_mounts, require_same_instance, start_backup_source
from capture_postgres import database_identity, local_exec
from capture_storage_identity import StorageIdentityError, require_local_docker, storage_identity


LEDGER_DESTINATION = "/var/lib/agent-pool-gateway/ledger-spool"


def spool_fingerprint(volume, guardian, max_bytes=16 * 1024 * 1024 * 1024):
    if not isinstance(volume, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", volume):
        raise BackupError("入帳磁碟卷來源不正確")
    process, nonce = start_backup_source(["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target=/spool,readonly", "--entrypoint", "tar", "python:3.13-slim", "-C", "/spool", "-cf", "-", "."])
    digest = hashlib.sha256()
    size = 0
    deadline = time.monotonic() + 3600
    try:
        while True:
            if guardian.poll() is not None:
                raise BackupError("備份協調程序已中斷")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise BackupError("入帳磁碟卷檢查逾時")
            ready, _, _ = select.select([process.stdout], [], [], min(5, remaining))
            if not ready:
                continue
            block = os.read(process.stdout.fileno(), 65536)
            if not block:
                break
            size += len(block)
            if size > max_bytes:
                raise BackupError("入帳磁碟卷超過檢查上限")
            digest.update(block)
        if not size or process.wait(timeout=10) != 0:
            raise BackupError("入帳磁碟卷無法檢查")
        return digest.hexdigest()
    finally:
        finish_backup_source(process, nonce)


def require_single_volume_owner(volume, gateway_id):
    if not isinstance(volume, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", volume) or not re.fullmatch(r"[0-9a-f]{64}", gateway_id):
        raise BackupError("入帳磁碟卷來源不正確")
    result = subprocess.run(["docker", "ps", "-aq", "--no-trunc"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=False)
    if result.returncode:
        raise BackupError("無法確認入帳磁碟卷使用者")
    try:
        containers = result.stdout.decode("ascii").splitlines()
    except UnicodeDecodeError as error:
        raise BackupError("無法確認入帳磁碟卷使用者") from error
    if not containers or len(containers) > 10000 or len(set(containers)) != len(containers) or any(not re.fullmatch(r"[0-9a-f]{64}", item) for item in containers):
        raise BackupError("無法確認入帳磁碟卷使用者")
    owners = set()
    for offset in range(0, len(containers), 100):
        batch = containers[offset:offset + 100]
        inspected = subprocess.run(["docker", "inspect", "--type=container", "--format", "{{.Id}} {{json .Mounts}}", *batch], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=20, check=False)
        if inspected.returncode:
            raise BackupError("無法確認入帳磁碟卷使用者")
        try:
            lines = inspected.stdout.decode("utf-8").splitlines()
            if len(lines) != len(batch):
                raise ValueError("container count changed")
            for expected, line in zip(batch, lines):
                identifier, raw = line.split(" ", 1)
                mounts = json.loads(raw)
                if identifier != expected or not isinstance(mounts, list) or any(not isinstance(mount, dict) for mount in mounts):
                    raise ValueError("container identity changed")
                if any(mount.get("Type") == "volume" and mount.get("Name") == volume for mount in mounts):
                    owners.add(identifier)
        except (UnicodeDecodeError, ValueError, TypeError) as error:
            raise BackupError("無法確認入帳磁碟卷使用者") from error
    if owners != {gateway_id}:
        raise BackupError("入帳磁碟卷有其他容器使用或來源不明")


def require_quiet_database(postgres_id, port, user, database):
    query = Path(__file__).resolve().parent.parent / "queries" / "ledger_backup_other_sessions.sql"
    with query.open("rb") as sql:
        result = subprocess.run(local_exec(postgres_id, port, "psql") + ["-U", user, "-d", database, "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=sql, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=False)
    if result.returncode or result.stdout.strip() != b"0":
        raise BackupError("資料庫仍有其他程序連線、待提交交易或無法確認")


def wal_position(postgres_id, port, user, database):
    query = Path(__file__).resolve().parent.parent / "queries" / "ledger_backup_wal_position.sql"
    with query.open("rb") as sql:
        result = subprocess.run(local_exec(postgres_id, port, "psql") + ["-U", user, "-d", database, "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=sql, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=False)
    position = result.stdout.strip()
    if result.returncode or not re.fullmatch(rb"[0-9A-F]{1,8}/[0-9A-F]{1,8}", position):
        raise BackupError("無法確認資料庫寫入紀錄位置")
    return position


def require_logged_relations(postgres_id, port, user, database):
    query = Path(__file__).resolve().parent.parent / "queries" / "ledger_backup_unlogged_relations.sql"
    with query.open("rb") as sql:
        result = subprocess.run(local_exec(postgres_id, port, "psql") + ["-U", user, "-d", database, "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=sql, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=False)
    if result.returncode or result.stdout.strip() != b"0":
        raise BackupError("資料庫含不寫入變更紀錄的資料或狀態不明")


def require_no_capture(postgres_id, port, user, database):
    query = Path(__file__).resolve().parent.parent / "queries" / "capture_backup_schema_present.sql"
    with query.open("rb") as sql:
        result = subprocess.run(local_exec(postgres_id, port, "psql") + ["-U", user, "-d", database, "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=sql, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=False)
    if result.returncode or result.stdout.strip() != b"f":
        raise BackupError("資料庫含全文保存資料或狀態不明")


def source(gateway, postgres, inspect_unconfirmed=False):
    require_local_docker()
    info, mounts = inspect_mounts(gateway, [LEDGER_DESTINATION])
    gateway_id = info["Id"]
    started = info["State"]["StartedAt"]
    environment = info["Config"]["Env"]
    paths = [entry.split("=", 1)[1] for entry in environment if entry.startswith("LEDGER_SPOOL_DIR=")]
    capture = [entry.split("=", 1)[1] for entry in environment if entry.startswith("CONTENT_CAPTURE_ENABLED=")]
    if paths != [LEDGER_DESTINATION] or capture not in ([], ["false"]) or (not capture and not inspect_unconfirmed):
        raise BackupError("無法確認入帳來源與全文保存設定")
    if not isinstance(gateway_id, str) or not re.fullmatch(r"[0-9a-f]{64}", gateway_id):
        raise BackupError("Gateway 識別不正確")
    user, database, postgres_id, postgres_started, port = database_target(info, gateway_id, postgres)
    if not isinstance(postgres_id, str) or not re.fullmatch(r"[0-9a-f]{64}", postgres_id):
        raise BackupError("資料庫容器識別不正確")
    identity = database_identity(postgres_id, port, user, database)
    require_no_capture(postgres_id, port, user, database)
    storage = storage_identity(mounts, expected=1)
    require_same_instance(gateway_id, started)
    require_same_instance(postgres_id, postgres_started)
    current, current_mounts = inspect_mounts(gateway_id, [LEDGER_DESTINATION])
    if current["Id"] != gateway_id or current["State"]["StartedAt"] != started or current["Config"]["Env"] != environment or database_target(current, gateway_id, postgres_id) != (user, database, postgres_id, postgres_started, port) or database_identity(postgres_id, port, user, database) != identity or storage_identity(current_mounts, expected=1) != storage:
        raise BackupError("備份來源已變更")
    require_no_capture(postgres_id, port, user, database)
    require_same_instance(gateway_id, started)
    require_same_instance(postgres_id, postgres_started)
    return {"gateway_id": gateway_id, "postgres_id": postgres_id, "database_system_id": identity[0], "database_oid": identity[1], "storage_identity": storage, "capture_setting": "disabled" if capture else "unconfirmed", "capture_disabled_setting_explicit": bool(capture)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--gateway", required=True)
    parser.add_argument("--postgres", required=True)
    parser.add_argument("--inspect-unconfirmed", action="store_true")
    args = parser.parse_args()
    try:
        result = source(args.gateway, args.postgres, inspect_unconfirmed=args.inspect_unconfirmed)
        print(json.dumps(result, sort_keys=True))
        if not result["capture_disabled_setting_explicit"]:
            print("全文保存狀態尚未確認，不可用於備份。", file=sys.stderr)
            return 2
    except (BackupError, StorageIdentityError, OSError, subprocess.SubprocessError, KeyError, IndexError, TypeError, ValueError):
        print("無法確認入帳備份來源。", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
