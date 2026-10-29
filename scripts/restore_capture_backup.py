import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import secrets
import shutil
import subprocess
import sys
import tarfile
import tempfile

from verify_capture_backup import FILES, VerifyError, open_regular, verify


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


def stage_backup(source, root, max_bytes):
    files = FILES | {"manifest.json", "manifest.sig", ".complete"}
    total = 0
    for name in sorted(files):
        with open_regular(source / name) as item:
            size = os.fstat(item.fileno()).st_size
            total += size
            if total > max_bytes:
                raise RestoreError("備份超過還原暫存上限")
            target = root / name
            descriptor = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "wb") as dest:
                shutil.copyfileobj(item, dest, 1024 * 1024)
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


def preflight(postgres, db_user, db_name, volumes):
    if not NAME.fullmatch(postgres) or not IDENTIFIER.fullmatch(db_user) or not IDENTIFIER.fullmatch(db_name):
        raise RestoreError("資料庫參數不正確")
    if len(set(volumes)) != 4 or any(not NAME.fullmatch(name) for name in volumes):
        raise RestoreError("還原磁碟卷不正確")
    details = json.loads(output(["docker", "inspect", postgres]))[0]
    if not details["State"]["Running"] or details["HostConfig"]["NetworkMode"] != "none":
        raise RestoreError("目標資料庫必須啟動且隔離網路")
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
    query = "SELECT (SELECT count(*) FROM pg_namespace WHERE nspname NOT IN ('public','pg_catalog','information_schema') AND left(nspname, 3) <> 'pg_') + (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public') + (SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='public') + (SELECT count(*) FROM pg_type t JOIN pg_namespace n ON n.oid=t.typnamespace WHERE n.nspname='public') + (SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND backend_type='client backend' AND pid<>pg_backend_pid())"
    value = output(["docker", "exec", postgres, "psql", "-U", db_user, "-d", db_name, "-Atqc", query]).strip()
    if value != b"0":
        raise RestoreError("目標資料庫不是空的或有其他連線")


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


def rotate_restored_key(keys, anchors, postgres, db_user, db_name, uid, gid):
    key = bytearray(secrets.token_bytes(32))
    try:
        fingerprint = hashlib.sha256(key).digest()
        name = fingerprint.hex()
        table = output(["docker", "exec", postgres, "psql", "-U", db_user, "-d", db_name, "-Atqc", "SELECT to_regclass('public.capture_master_key_usage') IS NOT NULL"]).strip()
        if table != b"t":
            raise RestoreError("備份缺少金鑰計數資料")
        used = output(["docker", "exec", postgres, "psql", "-U", db_user, "-d", db_name, "-Atqc", f"SELECT EXISTS (SELECT 1 FROM capture_master_key_usage WHERE key_fingerprint = decode('{name}', 'hex'))"]).strip()
        if used != b"f":
            raise RestoreError("新金鑰已存在於資料庫")
        key_mount = f"type=volume,source={keys},target=/keys"
        anchor_mount = f"type=volume,source={anchors},target=/anchors"
        output(["docker", "run", "--rm", "-i", "--pull=never", "--network", "none", "--mount", key_mount, "--entrypoint", "sh", IMAGE, "-c", f"umask 077; test ! -e /keys/{name}.key && dd bs=32 count=1 of=/keys/{name}.key 2>/dev/null && test \"$(wc -c < /keys/{name}.key)\" -eq 32 && chown {uid}:{gid} /keys/{name}.key && sync /keys/{name}.key && sync /keys",], bytes(key))
        body = b"APGANC01" + fingerprint + bytes(8) + b"\0"
        anchor = body + hashlib.sha256(body).digest()
        output(["docker", "run", "--rm", "-i", "--pull=never", "--network", "none", "--mount", anchor_mount, "--entrypoint", "sh", IMAGE, "-c", f"umask 077; test ! -e /anchors/{name}.anchor && dd bs=81 count=1 of=/anchors/{name}.anchor 2>/dev/null && test \"$(wc -c < /anchors/{name}.anchor)\" -eq 81 && chown {uid}:{gid} /anchors/{name}.anchor && chmod 700 /anchors && sync /anchors/{name}.anchor && sync /anchors",], anchor)
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
        verify(staged, args.signer)
        check_archives(staged, args.max_bytes)
        preflight(args.postgres, args.db_user, args.db_name, volumes)
        for name, volume in [("capture-spool.tar.gpg", args.spool), ("capture-keys.tar.gpg", args.keys), ("ledger-spool.tar.gpg", args.ledger)]:
            tar_restore(staged / name, volume, args.uid, args.gid)
        decrypt_to(staged / "gateway.dump.gpg", ["docker", "exec", "-i", args.postgres, "pg_restore", "-U", args.db_user, "-d", args.db_name, "--exit-on-error", "--single-transaction"])
        return rotate_restored_key(args.keys, args.anchors, args.postgres, args.db_user, args.db_name, args.uid, args.gid)


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
