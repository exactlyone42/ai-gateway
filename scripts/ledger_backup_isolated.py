import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import select
import stat
import subprocess
import sys
import tempfile
import time

from capture_backup import BackupError, database_target, inspect_mounts
from capture_postgres import database_identity, local_exec
from capture_storage_identity import StorageIdentityError, storage_identity
from ledger_backup_bundle import write_bundle
from ledger_backup_source import LEDGER_DESTINATION, require_logged_relations, require_no_capture, require_quiet_database, require_single_volume_owner, source, spool_fingerprint, wal_position
from verify_capture_backup import VerifyError, verify_signature


def inspect(identifier):
    result = subprocess.run(["docker", "inspect", identifier], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=True)
    return json.loads(result.stdout)[0]


def process_state(pid):
    try:
        data = (Path("/proc") / str(pid) / "stat").read_text()
    except FileNotFoundError:
        return None
    fields = data.rpartition(") ")[2].split()
    if len(fields) < 20:
        raise BackupError("停止程序狀態不明")
    return fields[0], fields[19]


def wait_stop_process(pid, started):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        current = process_state(pid)
        if current is None or current[0] == "Z" or (started is not None and current[1] != started):
            return
        time.sleep(0.1)
    raise BackupError("停止程序仍在執行，未嘗試重新啟動")


def require_destination_and_keys(destination, recipient, signer):
    if not all(isinstance(value, str) and re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", value) for value in (recipient, signer)):
        raise BackupError("測試金鑰識別值不正確")
    directory = os.open(destination.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        metadata = os.fstat(directory)
        if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
            raise BackupError("測試備份目錄權限不正確")
        try:
            os.stat(destination.name, dir_fd=directory, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise BackupError("測試備份目的地已存在")
    finally:
        os.close(directory)
    for option, fingerprint in (("--list-keys", recipient), ("--list-secret-keys", signer)):
        result = subprocess.run(["gpg", "--batch", "--no-auto-key-retrieve", option, fingerprint], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=False)
        if result.returncode:
            raise BackupError("測試備份金鑰無法使用")
    challenge = os.urandom(32)
    encrypted = subprocess.run(["gpg", "--batch", "--no-auto-key-retrieve", "--no-encrypt-to", "--trust-model", "always", "--recipient", recipient, "--encrypt"], input=challenge, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=20, check=False)
    if encrypted.returncode or not encrypted.stdout:
        raise BackupError("測試備份加密金鑰無法加密")
    signed = subprocess.run(["gpg", "--batch", "--no-auto-key-retrieve", "--local-user", signer, "--detach-sign", "--output", "-"], input=challenge, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=20, check=False)
    if signed.returncode or not signed.stdout:
        raise BackupError("測試備份簽署金鑰無法簽署")
    with tempfile.TemporaryDirectory(prefix="ledger-key-check-") as directory:
        signature = Path(directory) / "manifest.sig"
        signature.write_bytes(signed.stdout)
        signature.chmod(0o600)
        try:
            verify_signature(Path(directory), challenge, signer)
        except VerifyError as error:
            raise BackupError("測試備份簽署金鑰無法驗證") from error


def backup(args):
    if not args.destination.is_absolute() or not args.lock.is_absolute():
        raise BackupError("測試目的地與鎖路徑必須是絕對路徑")
    descriptor = os.open(args.lock, os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    guardian = None
    stopped_confirmed = False
    stop_pid = None
    stop_started = None
    parent_directory = None
    destination_directory = None
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        require_destination_and_keys(args.destination, args.recipient, args.signer)
        identity = source(args.gateway, args.postgres)
        gateway, mounts = inspect_mounts(identity["gateway_id"], [LEDGER_DESTINATION])
        if (gateway["Config"].get("Labels") or {}).get("gateway.ledger_watchdog_test") != args.nonce:
            raise BackupError("僅支援專屬隔離測試容器")
        parent_directory = os.open(args.destination.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        parent_metadata = os.fstat(parent_directory)
        if parent_metadata.st_uid != os.getuid() or parent_metadata.st_mode & 0o077:
            raise BackupError("測試備份目錄權限不正確")
        os.mkdir(args.destination.name, mode=0o700, dir_fd=parent_directory)
        destination_directory = os.open(args.destination.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent_directory)
        pinned_destination = Path(f"/proc/self/fd/{destination_directory}")

        def check_destination():
            current = os.stat(args.destination.name, dir_fd=parent_directory, follow_symlinks=False)
            expected = os.fstat(destination_directory)
            if not stat.S_ISDIR(current.st_mode) or (current.st_dev, current.st_ino) != (expected.st_dev, expected.st_ino):
                raise BackupError("測試備份目的地已被替換")

        user, database, postgres_id, postgres_started, port = database_target(gateway, identity["gateway_id"], identity["postgres_id"])
        volume = mounts[0]["Name"]
        gateway_started = gateway["State"]["StartedAt"]
        environment = gateway["Config"]["Env"]
        guardian = subprocess.Popen([sys.executable, str(Path(__file__).with_name("ledger_watchdog_fixture.py")), identity["gateway_id"], args.nonce, f"fd:{destination_directory}"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, pass_fds=(descriptor, destination_directory), start_new_session=True, bufsize=0)
        if not select.select([guardian.stdout], [], [], 30)[0]:
            raise BackupError("隔離測試停止程序無回應")
        message = guardian.stdout.readline().strip().split()
        if len(message) != 2 or message[0] != b"STOPPING" or not message[1].isdigit() or int(message[1]) <= 0:
            raise BackupError("隔離測試停止程序狀態不明")
        stop_pid = int(message[1])
        stop_state = process_state(stop_pid)
        stop_started = stop_state[1] if stop_state is not None else None
        if not select.select([guardian.stdout], [], [], 30)[0] or guardian.stdout.readline().strip() != b"STOPPED":
            raise BackupError("隔離測試服務無法正常停止")
        stopped_confirmed = True

        def check_source():
            check_destination()
            current = inspect(identity["gateway_id"])
            state = current["State"]
            selected = [mount for mount in current["Mounts"] if mount.get("Destination") == LEDGER_DESTINATION]
            if current["Id"] != identity["gateway_id"] or state.get("Status") != "exited" or state.get("ExitCode") != 0 or state.get("StartedAt") != gateway_started or current["Config"]["Env"] != environment or len(selected) != 1 or storage_identity(selected, expected=1) != identity["storage_identity"]:
                raise BackupError("隔離測試 Gateway 來源已改變")
            db = inspect(postgres_id)
            if db["Id"] != postgres_id or not db["State"]["Running"] or db["State"]["StartedAt"] != postgres_started or database_identity(postgres_id, port, user, database) != (identity["database_system_id"], identity["database_oid"]):
                raise BackupError("隔離測試資料庫來源已改變")
            require_no_capture(postgres_id, port, user, database)
            require_logged_relations(postgres_id, port, user, database)
            require_quiet_database(postgres_id, port, user, database)
            require_single_volume_owner(volume, identity["gateway_id"])

        check_source()
        initial_wal = wal_position(postgres_id, port, user, database)
        initial_spool = spool_fingerprint(volume, guardian)

        def final_check():
            check_source()
            if wal_position(postgres_id, port, user, database) != initial_wal:
                raise BackupError("備份期間資料庫寫入紀錄已改變")
            if spool_fingerprint(volume, guardian) != initial_spool:
                raise BackupError("備份期間入帳磁碟卷內容已改變")

        producers = {"gateway.dump.gpg": local_exec(postgres_id, port, "pg_dump") + ["-U", user, "-d", database, "-Fc"], "ledger-spool.tar.gpg": ["docker", "run", "--rm", "--label", "gateway.ledger_watchdog_test=" + args.nonce, "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target=/spool,readonly", "--entrypoint", "tar", "python:3.13-slim", "-C", "/spool", "-cf", "-", "."]}
        bundle_identity = {key: identity[key] for key in ("gateway_id", "postgres_id", "database_system_id", "database_oid", "storage_identity")}
        write_bundle(pinned_destination, producers, args.recipient, args.signer, bundle_identity, guardian, final_check=final_check, expected_hashes={"ledger-spool.tar.gpg": initial_spool}, precreated=True)
        if guardian.stdout.read().strip() != b"RESTARTED":
            raise BackupError("隔離測試服務重啟狀態不明")
        check_destination()
    finally:
        if guardian is not None:
            if guardian.stdin and not guardian.stdin.closed:
                guardian.stdin.close()
            if guardian.poll() is None:
                guardian.wait(timeout=60)
            if (stopped_confirmed or stop_pid is not None) and guardian.returncode != 0:
                if not stopped_confirmed:
                    wait_stop_process(stop_pid, stop_started)
                current = inspect(identity["gateway_id"])
                state = current["State"]
                labels = current["Config"].get("Labels") or {}
                if current["Id"] != identity["gateway_id"] or labels.get("gateway.ledger_watchdog_test") != args.nonce or state.get("StartedAt") != gateway_started:
                    raise BackupError("隔離測試服務身分已改變，未嘗試重新啟動")
                if state.get("Status") == "exited":
                    subprocess.run(["docker", "start", identity["gateway_id"]], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
                elif not state.get("Running"):
                    raise BackupError("隔離測試服務狀態不明，未嘗試重新啟動")
            if guardian.stdout:
                guardian.stdout.close()
        if destination_directory is not None:
            os.close(destination_directory)
        if parent_directory is not None:
            os.close(parent_directory)
        os.close(descriptor)


def main():
    parser = argparse.ArgumentParser(description="僅供專屬標籤的隔離容器演練")
    parser.add_argument("--gateway", required=True)
    parser.add_argument("--postgres", required=True)
    parser.add_argument("--nonce", required=True)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--lock", type=Path, required=True)
    parser.add_argument("--recipient", required=True)
    parser.add_argument("--signer", required=True)
    args = parser.parse_args()
    try:
        backup(args)
    except BackupError as error:
        print("隔離備份演練失敗：" + str(error), file=sys.stderr)
        return 1
    except (StorageIdentityError, OSError, ValueError, KeyError, IndexError, subprocess.SubprocessError):
        print("隔離備份演練失敗，來源服務狀態需另行核對。", file=sys.stderr)
        return 1
    print("隔離備份演練結束，仍需驗證備份及服務就緒。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
