import argparse
import re
import subprocess
import sys

from capture_backup import BackupError, database_target, inspect_mounts


def verify(gateway, postgres, volume):
    destination = "/var/lib/agent-pool-gateway/ledger-spool"
    info, mounts = inspect_mounts(gateway, [destination])
    if mounts[0].get("Type") != "volume" or mounts[0].get("Name") != volume:
        raise BackupError("Gateway 入帳磁碟卷與備份來源不一致")
    paths = [entry.split("=", 1)[1] for entry in info["Config"]["Env"] if entry.startswith("LEDGER_SPOOL_DIR=")]
    if paths != [destination]:
        raise BackupError("Gateway 入帳路徑與備份來源不一致")
    user, database, postgres_id, postgres_started, postgres_port = database_target(info, info["Id"], postgres)
    if user != "gateway" or database != "gateway":
        raise BackupError("舊版備份來源與 Gateway 資料庫不一致")
    gateway_id = info["Id"]
    gateway_started = info["State"]["StartedAt"]
    if any(not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value) for value in (gateway_id, postgres_id)):
        raise BackupError("容器識別不正確")
    if any(not isinstance(value, str) or not re.fullmatch(r"[0-9TZ:.-]+", value) for value in (gateway_started, postgres_started)):
        raise BackupError("容器啟動時間不正確")
    return gateway_id, postgres_id, gateway_started, postgres_started, str(postgres_port)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--gateway", required=True)
    parser.add_argument("--postgres", required=True)
    parser.add_argument("--volume", required=True)
    args = parser.parse_args()
    try:
        identity = verify(args.gateway, args.postgres, args.volume)
    except (BackupError, OSError, subprocess.SubprocessError, KeyError, TypeError, ValueError):
        print("無法確認 Gateway 資料庫來源，未開始操作。", file=sys.stderr)
        return 1
    print(" ".join(identity))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
