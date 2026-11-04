import argparse
from pathlib import Path
import re
import subprocess
import sys

from capture_backup import BackupError, database_target, inspect_mounts, require_same_instance, validate_storage_environment
from capture_postgres import database_identity, local_exec
from capture_storage_identity import StorageIdentityError, storage_identity


def state(gateway, postgres):
    info, _ = inspect_mounts(gateway, [])
    gateway_id = info["Id"]
    if not isinstance(gateway_id, str) or not re.fullmatch(r"[0-9a-f]{64}", gateway_id):
        raise BackupError("Gateway 識別不正確")
    gateway_started = info["State"]["StartedAt"]
    user, database, postgres_id, postgres_started, port = database_target(info, gateway_id, postgres)
    if not isinstance(postgres_id, str) or not re.fullmatch(r"[0-9a-f]{64}", postgres_id):
        raise BackupError("資料庫容器識別不正確")
    query = Path(__file__).resolve().parent.parent / "queries" / "capture_backup_schema_present.sql"
    with query.open("rb") as source:
        result = subprocess.run(local_exec(postgres_id, port, "psql") + ["-U", user, "-d", database, "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=source, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=False)
    if result.returncode or result.stdout.strip() not in (b"t", b"f"):
        raise BackupError("無法確認資料庫保存狀態")
    identity = database_identity(postgres_id, port, user, database)
    require_same_instance(gateway_id, gateway_started)
    require_same_instance(postgres_id, postgres_started)
    current, _ = inspect_mounts(gateway_id, [])
    if database_target(current, gateway_id, postgres_id) != (user, database, postgres_id, postgres_started, port):
        raise BackupError("資料庫來源已變更")
    capture_state = result.stdout.strip().decode("ascii")
    storage = "-"
    if capture_state == "t":
        keys = ("CONTENT_CAPTURE_SPOOL_DIR", "CONTENT_CAPTURE_KEY_DIR", "LEDGER_SPOOL_DIR", "CONTENT_CAPTURE_ERASURE_DIR", "CONTENT_CAPTURE_WITNESS_DIR")
        destinations = []
        for key in keys:
            values = [entry.split("=", 1)[1] for entry in current["Config"]["Env"] if entry.startswith(key + "=")]
            if len(values) != 1 or not values[0]:
                raise BackupError("保存路徑未明確設定")
            destinations.append(values[0])
        current, mounts = inspect_mounts(gateway_id, destinations)
        validate_storage_environment(current, destinations)
        storage = storage_identity(mounts)
        require_same_instance(gateway_id, gateway_started)
        require_same_instance(postgres_id, postgres_started)
    return capture_state, gateway_id, postgres_id, *identity, storage


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--gateway", required=True)
    parser.add_argument("--postgres", required=True)
    args = parser.parse_args()
    try:
        print(*state(args.gateway, args.postgres))
    except (BackupError, StorageIdentityError, OSError, subprocess.SubprocessError, KeyError, IndexError, TypeError, ValueError):
        print("無法確認備份資料來源。", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
