import argparse
from datetime import datetime, timedelta
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import secrets
import selectors
import subprocess
import sys
import tarfile
import tempfile
import time

from verify_capture_backup import FILES, VerifyError, open_regular, verify
from capture_postgres import configured_port, local_exec


IMAGE = "postgres:17-alpine"
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}\Z")
IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_-]{0,62}\Z")


class RestoreError(Exception):
    pass


def output(args, input_data=None):
    result = subprocess.run(args, input=input_data, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=60, check=False)
    if result.returncode:
        raise RestoreError("還原前檢查失敗")
    return result.stdout


def stage_backup(source, root, max_bytes, data_files=FILES):
    files = data_files | {"manifest.json", "manifest.sig", ".complete"}
    total = 0
    for name in sorted(files):
        with open_regular(source / name) as item:
            size = os.fstat(item.fileno()).st_size
            if size > max_bytes - total:
                raise RestoreError("備份超過還原暫存上限")
            target = root / name
            descriptor = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "wb") as dest:
                copied = 0
                while block := item.read(1024 * 1024):
                    copied += len(block)
                    if copied > size:
                        raise RestoreError("備份複製期間大小已改變")
                    dest.write(block)
                if copied != size:
                    raise RestoreError("備份複製期間大小已改變")
                total += copied
                dest.flush()
                os.fsync(dest.fileno())


def check_archives(root, max_bytes):
    entries = 0
    total = 0
    key_names = set()
    current = None
    for name in ("capture-spool.tar.gpg", "capture-keys.tar.gpg", "ledger-spool.tar.gpg"):
        with open_regular(root / name) as source:
            decrypt = subprocess.Popen(["gpg", "--batch", "--no-auto-key-retrieve", "--decrypt"], stdin=source, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            try:
                with tarfile.open(fileobj=decrypt.stdout, mode="r|*") as archive:
                    seen = set()
                    for item in archive:
                        entries += 1
                        total += item.size
                        if entries > 100000 or total > max_bytes:
                            raise RestoreError("備份內容超過還原上限")
                        path = PurePosixPath(item.name)
                        if path.is_absolute() or ".." in path.parts or "\x00" in item.name or not (item.isfile() or item.isdir()):
                            raise RestoreError("備份包含不安全的路徑或檔案")
                        normalized = str(path)
                        if normalized in seen:
                            raise RestoreError("備份包含重複檔案")
                        seen.add(normalized)
                        if item.isdir():
                            if normalized != ".":
                                raise RestoreError("備份包含不預期的子目錄")
                            continue
                        if len(path.parts) != 1:
                            raise RestoreError("備份包含不預期的子目錄")
                        if name == "capture-spool.tar.gpg":
                            if not re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.capture|\.capture-capacity\.lock|\.capture-backup\.lock", normalized):
                                raise RestoreError("全文暫存包含不預期的檔案")
                        elif name == "capture-keys.tar.gpg":
                            if item.mode & 0o077:
                                raise RestoreError("金鑰檔案權限不正確")
                            if re.fullmatch(r"[0-9a-f]{64}\.key", normalized):
                                key = archive.extractfile(item).read(33)
                                if len(key) != 32 or hashlib.sha256(key).hexdigest() != normalized[:-4]:
                                    raise RestoreError("金鑰檔案不正確")
                                key_names.add(normalized[:-4])
                            elif normalized == "current":
                                current = archive.extractfile(item).read(33)
                                if len(current) != 32:
                                    raise RestoreError("目前金鑰不正確")
                            elif normalized != ".rotation.lock":
                                raise RestoreError("金鑰目錄包含不預期的檔案")
                decrypt.stdout.close()
                if decrypt.wait(timeout=10):
                    raise RestoreError("備份無法解密")
            finally:
                if decrypt.stdout:
                    decrypt.stdout.close()
                if decrypt.poll() is None:
                    decrypt.terminate()
                    decrypt.wait(timeout=10)
    if current is None or current.hex() not in key_names:
        raise RestoreError("目前金鑰不存在")


def preflight(postgres, db_user, db_name, volumes, expected_volumes=4):
    if not NAME.fullmatch(postgres) or not IDENTIFIER.fullmatch(db_user) or not IDENTIFIER.fullmatch(db_name):
        raise RestoreError("資料庫參數不正確")
    if type(expected_volumes) is not int or expected_volumes not in (1, 4) or len(volumes) != expected_volumes or len(set(volumes)) != expected_volumes or any(not NAME.fullmatch(name) or re.fullmatch(r"[0-9a-f]{64}", name) for name in volumes):
        raise RestoreError("還原磁碟卷不正確")
    details = json.loads(output(["docker", "inspect", postgres]))[0]
    if not details["State"]["Running"] or details["HostConfig"]["NetworkMode"] != "none":
        raise RestoreError("目標資料庫必須啟動且隔離網路")
    target_id = details["Id"]
    started = details["State"]["StartedAt"]
    port = configured_port(details)
    ids = output(["docker", "ps", "-aq"]).decode("ascii").splitlines()
    for container in ids:
        mounts = json.loads(output(["docker", "inspect", "--format", "{{json .Mounts}}", container]))
        if any(mount.get("Type") == "volume" and mount.get("Name") in volumes for mount in mounts):
            raise RestoreError("還原磁碟卷已被容器使用")
    for volume in volumes:
        details = json.loads(output(["docker", "volume", "inspect", volume]))
        if len(details) != 1 or details[0].get("Name") != volume:
            raise RestoreError("還原磁碟卷不存在")
        output(["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target=/target", "--entrypoint", "sh", IMAGE, "-c", "test -z \"$(ls -A /target)\""])
    query = (Path(__file__).resolve().parent.parent / "queries/capture_restore_preflight.sql").read_bytes()
    value = output(local_exec(target_id, port, "psql") + ["-U", db_user, "-d", db_name, "-Atq", "-v", "ON_ERROR_STOP=1"], query).strip()
    if value != b"0":
        raise RestoreError("目標資料庫不是空的或有其他連線")
    return target_id, started, port


def require_same_target(identifier, started):
    details = json.loads(output(["docker", "inspect", identifier]))
    if len(details) != 1 or details[0]["Id"] != identifier or not details[0]["State"]["Running"] or details[0]["State"]["StartedAt"] != started or details[0]["HostConfig"]["NetworkMode"] != "none":
        raise RestoreError("還原期間目標資料庫已變更")


def check_dump_expansion(path, target, max_bytes):
    details = json.loads(output(["docker", "inspect", target]))[0]
    image = details["Image"]
    expiry = (datetime.now() + timedelta(days=1)).date().isoformat()
    container = output([
        "docker", "create", "-i", "--pull=never", "--network", "none",
        "--label", "expire=" + expiry,
        "--label", "agent-pool-gateway.capture-preflight=1",
        "--log-driver", "none", "--read-only", "--cap-drop", "ALL",
        "--security-opt", "no-new-privileges", "--user", "65534",
        "--memory", "256m", "--pids-limit", "32",
        "--tmpfs", "/var/lib/postgresql/data", "--entrypoint", "pg_restore",
        image, "--exit-on-error", "-f", "-",
    ]).decode("ascii").strip()
    if not re.fullmatch(r"[0-9a-f]{64}", container):
        raise RestoreError("無法建立還原檢查容器")
    decrypt = None
    convert = None
    try:
        with open_regular(path) as source:
            decrypt = subprocess.Popen(["gpg", "--batch", "--no-auto-key-retrieve", "--decrypt"], stdin=source, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            convert = subprocess.Popen(["docker", "start", "-a", "-i", container], stdin=decrypt.stdout, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            decrypt.stdout.close()
            deadline = time.monotonic() + 3600
            total = 0
            with selectors.DefaultSelector() as selector:
                selector.register(convert.stdout, selectors.EVENT_READ)
                while True:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0 or not selector.select(remaining):
                        raise RestoreError("資料庫備份展開檢查逾時")
                    block = os.read(convert.stdout.fileno(), 65536)
                    if not block:
                        break
                    total += len(block)
                    if total > max_bytes:
                        raise RestoreError("資料庫備份展開超過還原上限")
            if convert.wait(timeout=max(1, deadline - time.monotonic())) or decrypt.wait(timeout=max(1, deadline - time.monotonic())):
                raise RestoreError("資料庫備份展開檢查失敗")
    finally:
        cleanup_failed = False
        for process in (convert, decrypt):
            if process is not None and process.poll() is None:
                try:
                    process.kill()
                    process.wait(timeout=10)
                except (OSError, subprocess.SubprocessError):
                    cleanup_failed = True
        if convert is not None and convert.stdout:
            convert.stdout.close()
        try:
            removed = subprocess.run(["docker", "rm", "-f", "-v", container], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=30, check=False)
            cleanup_failed |= bool(removed.returncode and b"No such container" not in removed.stderr)
        except (OSError, subprocess.SubprocessError):
            cleanup_failed = True
        if cleanup_failed:
            raise RestoreError("無法清理資料庫備份檢查容器")


def decrypt_to(path, destination):
    with open_regular(path) as source:
        decrypt = subprocess.Popen(["gpg", "--batch", "--no-auto-key-retrieve", "--decrypt"], stdin=source, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        try:
            result = subprocess.run(destination, stdin=decrypt.stdout, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=3600, check=False)
            decrypt.stdout.close()
            if result.returncode or decrypt.wait(timeout=10):
                raise RestoreError("還原資料失敗")
        finally:
            if decrypt.stdout:
                decrypt.stdout.close()
            if decrypt.poll() is None:
                decrypt.terminate()
                decrypt.wait(timeout=10)


def tar_restore(path, volume, uid, gid):
    mount = f"type=volume,source={volume},target=/target"
    decrypt_to(path, ["docker", "run", "--rm", "-i", "--pull=never", "--network", "none", "--mount", mount, "--entrypoint", "tar", IMAGE, "-xf", "-", "-C", "/target"])
    output(["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", mount, "--entrypoint", "sh", IMAGE, "-c", f"chown -R {uid}:{gid} /target && chmod 700 /target && sync -f /target"])


def rotate_restored_key(keys, anchors, postgres, port, db_user, db_name, uid, gid):
    key = bytearray(secrets.token_bytes(32))
    try:
        fingerprint = hashlib.sha256(key).digest()
        name = fingerprint.hex()
        root = Path(__file__).resolve().parent.parent
        psql = local_exec(postgres, port, "psql") + ["-U", db_user, "-d", db_name, "-Atq", "-v", "ON_ERROR_STOP=1"]
        table = output(psql, (root / "queries/capture_restore_usage_exists.sql").read_bytes()).strip()
        if table != b"t":
            raise RestoreError("備份缺少金鑰計數資料")
        used = output(psql + ["-v", "fingerprint=" + name], (root / "queries/capture_restore_key_used.sql").read_bytes()).strip()
        if used != b"f":
            raise RestoreError("新金鑰已存在於資料庫")
        key_mount = f"type=volume,source={keys},target=/keys"
        anchor_mount = f"type=volume,source={anchors},target=/anchors"
        output(["docker", "run", "--rm", "-i", "--pull=never", "--network", "none", "--mount", key_mount, "--entrypoint", "sh", IMAGE, "-c", f"umask 077; test ! -e /keys/{name}.key && dd bs=32 count=1 of=/keys/{name}.key 2>/dev/null && test \"$(wc -c < /keys/{name}.key)\" -eq 32 && chown {uid}:{gid} /keys/{name}.key && sync /keys/{name}.key && sync /keys",], bytes(key))
        body = b"APGANC01" + fingerprint + bytes(8) + b"\0"
        anchor = body + hashlib.sha256(body).digest()
        output(["docker", "run", "--rm", "-i", "--pull=never", "--network", "none", "--mount", anchor_mount, "--entrypoint", "sh", IMAGE, "-c", f"umask 077; test ! -e /anchors/{name}.anchor && dd bs=81 count=1 of=/anchors/{name}.anchor 2>/dev/null && test \"$(wc -c < /anchors/{name}.anchor)\" -eq 81 && chown {uid}:{gid} /anchors /anchors/{name}.anchor && chmod 700 /anchors && sync /anchors/{name}.anchor && sync /anchors",], anchor)
        temporary = f".current-restored-{secrets.token_hex(16)}"
        output(["docker", "run", "--rm", "-i", "--pull=never", "--network", "none", "--mount", key_mount, "--entrypoint", "sh", IMAGE, "-c", f"umask 077; test ! -e /keys/{temporary} && dd bs=32 count=1 of=/keys/{temporary} 2>/dev/null && test \"$(wc -c < /keys/{temporary})\" -eq 32 && chown {uid}:{gid} /keys/{temporary} && sync /keys/{temporary} && mv /keys/{temporary} /keys/current && sync /keys",], fingerprint)
        return name
    finally:
        key[:] = bytes(len(key))


def restore(args):
    if args.max_bytes <= 0 or args.uid < 1 or args.gid < 1:
        raise RestoreError("還原限制不正確")
    volumes = [args.spool, args.keys, args.ledger, args.anchors]
    with tempfile.TemporaryDirectory(prefix="capture-restore-", dir=args.staging) as directory:
        staged = Path(directory)
        stage_backup(args.backup, staged, args.max_bytes)
        manifest = verify(staged, args.signer)
        check_archives(staged, args.max_bytes)
        target_id, target_started, target_port = preflight(args.postgres, args.db_user, args.db_name, volumes)
        check_dump_expansion(staged / "gateway.dump.gpg", target_id, args.max_bytes)
        manifest = verify(staged, args.signer)
        require_same_target(target_id, target_started)
        root = Path(__file__).resolve().parent.parent
        psql = local_exec(target_id, target_port, "psql") + ["-U", args.db_user, "-d", args.db_name, "-v", "ON_ERROR_STOP=1"]
        output(psql, (root / "queries/capture_restore_begin.sql").read_bytes())
        for name, volume in [("capture-spool.tar.gpg", args.spool), ("capture-keys.tar.gpg", args.keys), ("ledger-spool.tar.gpg", args.ledger)]:
            tar_restore(staged / name, volume, args.uid, args.gid)
        decrypt_to(staged / "gateway.dump.gpg", local_exec(target_id, target_port, "pg_restore") + ["-U", args.db_user, "-d", args.db_name, "--exit-on-error", "--single-transaction"])
        fingerprint = rotate_restored_key(args.keys, args.anchors, target_id, target_port, args.db_user, args.db_name, args.uid, args.gid)
        require_same_target(target_id, target_started)
        position = manifest["erasure_position"]
        variables = ["-v", "install=" + position["install"], "-v", "sequence=" + str(position["sequence"]), "-v", "tip=" + bytes(position["tip"]).hex()]
        output(psql + variables, (root / "queries/capture_restore_finish.sql").read_bytes())
        return fingerprint


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--backup", type=Path, required=True)
    parser.add_argument("--signer", required=True)
    parser.add_argument("--postgres", required=True)
    parser.add_argument("--spool", required=True)
    parser.add_argument("--keys", required=True)
    parser.add_argument("--ledger", required=True)
    parser.add_argument("--anchors", required=True)
    parser.add_argument("--db-user", default="gateway")
    parser.add_argument("--db-name", default="gateway")
    parser.add_argument("--uid", type=int, default=10001)
    parser.add_argument("--gid", type=int, default=999)
    parser.add_argument("--max-bytes", type=int, default=16 * 1024 * 1024 * 1024)
    parser.add_argument("--staging", type=Path)
    args = parser.parse_args()
    try:
        fingerprint = restore(args)
    except (RestoreError, VerifyError, OSError, subprocess.SubprocessError, ValueError, KeyError, IndexError, tarfile.TarError):
        print("還原失敗；原備份未變更。目標可能不完整，請勿啟動服務。", file=sys.stderr)
        return 1
    print(f"已還原至隔離目標並建立新金鑰：{fingerprint}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
