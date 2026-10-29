import argparse
import datetime
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import select
import subprocess
import sys
import time
from urllib.parse import unquote, urlsplit


IMAGE = "postgres:17-alpine"


class BackupError(Exception):
    pass


def command(args):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=15, check=False)
    if result.returncode:
        raise BackupError("無法檢查備份來源")
    return result.stdout


def mount_spec(mount, target, readonly):
    kind = mount.get("Type")
    source = mount.get("Name") if kind == "volume" else mount.get("Source")
    if kind not in ("volume", "bind") or not isinstance(source, str) or not source or "," in source:
        raise BackupError("備份來源掛載不正確")
    if kind == "bind" and not Path(source).is_absolute():
        raise BackupError("備份來源掛載不正確")
    value = f"type={kind},source={source},target={target}"
    return value + ",readonly" if readonly else value


def inspect_mounts(container, destinations):
    try:
        details = json.loads(command(["docker", "inspect", container]))
        item = details[0]
        if not item["State"]["Running"]:
            raise BackupError("Gateway 未執行")
        mounts = item["Mounts"]
        found = []
        for destination in destinations:
            matches = [mount for mount in mounts if mount.get("Destination") == destination]
            if len(matches) != 1:
                raise BackupError("Gateway 缺少必要掛載")
            found.append(matches[0])
        sources = [(mount.get("Type"), mount.get("Name") or mount.get("Source")) for mount in found]
        if len(set(sources)) != len(sources):
            raise BackupError("備份來源掛載重複")
        return item, found
    except (KeyError, IndexError, TypeError, ValueError) as error:
        raise BackupError("無法核對 Gateway 掛載") from error


def database_target(gateway, gateway_name, postgres):
    try:
        targets = json.loads(command(["docker", "inspect", postgres]))
        target = targets[0]
        if not target["State"]["Running"]:
            raise BackupError("資料庫未執行")
        values = [entry.split("=", 1)[1] for entry in gateway["Config"]["Env"] if entry.startswith("DATABASE_URL=")]
        if len(values) != 1:
            raise BackupError("無法核對資料庫來源")
        parsed = urlsplit(values[0])
        if parsed.scheme not in ("postgres", "postgresql") or not parsed.hostname or not parsed.username or not parsed.path[1:]:
            raise BackupError("無法核對資料庫來源")
        gateway_networks = gateway["NetworkSettings"]["Networks"]
        target_networks = target["NetworkSettings"]["Networks"]
        candidates = set()
        for network in set(gateway_networks).intersection(target_networks):
            details = target_networks[network]
            names = set(details.get("Aliases") or [])
            names.add(details.get("IPAddress", ""))
            names.add(target.get("Name", "").lstrip("/"))
            if parsed.hostname in names:
                address = details.get("IPAddress", "")
                if address:
                    candidates.add(str(ipaddress.ip_address(address)))
        if len(candidates) != 1:
            raise BackupError("指定的資料庫不是 Gateway 的資料庫")
        resolved = command(["docker", "exec", gateway_name, "getent", "ahostsv4", parsed.hostname]).decode("ascii")
        addresses = {str(ipaddress.ip_address(line.split()[0])) for line in resolved.splitlines() if line.split()}
        if addresses != candidates:
            raise BackupError("資料庫名稱解析不唯一或與指定容器不符")
        return unquote(parsed.username), unquote(parsed.path[1:])
    except (KeyError, IndexError, TypeError, ValueError) as error:
        raise BackupError("無法核對資料庫來源") from error


def encrypt_stream(producer, output, recipient, lock, prefix):
    source = subprocess.Popen(producer, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    encrypted_process = None
    try:
        with output.open("xb") as encrypted:
            os.chmod(output, 0o600)
            encrypted_process = subprocess.Popen(
                ["gpg", "--batch", "--no-auto-key-retrieve", "--yes", "--trust-model", "always", "--recipient", recipient, "--encrypt"],
                stdin=subprocess.PIPE, stdout=encrypted, stderr=subprocess.DEVNULL,
            )
            deadline = time.monotonic() + 3600
            first = bytearray()
            validated = False
            while True:
                if lock.poll() is not None:
                    raise BackupError("備份協調鎖已中斷")
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise BackupError("備份來源逾時")
                ready, _, _ = select.select([source.stdout], [], [], min(5, remaining))
                if not ready:
                    continue
                block = os.read(source.stdout.fileno(), 65536)
                if not block:
                    break
                if not validated:
                    first.extend(block)
                    needed = len(prefix) if prefix else 512
                    if len(first) < needed:
                        continue
                    if prefix and not first.startswith(prefix):
                        raise BackupError("備份來源格式不正確")
                    encrypted_process.stdin.write(first)
                    first[:] = bytes(len(first))
                    first.clear()
                    validated = True
                else:
                    encrypted_process.stdin.write(block)
            if not validated:
                raise BackupError("備份來源為空或格式不正確")
            encrypted_process.stdin.close()
            source_status = source.wait()
            encrypted_status = encrypted_process.wait()
            if encrypted_status or source_status:
                raise BackupError("備份加密失敗")
            encrypted.flush()
            os.fsync(encrypted.fileno())
    finally:
        if source.stdout:
            source.stdout.close()
        if encrypted_process and encrypted_process.stdin:
            try:
                encrypted_process.stdin.close()
            except BrokenPipeError:
                pass
        if source.poll() is None:
            source.terminate()
            source.wait()
        if encrypted_process and encrypted_process.poll() is None:
            encrypted_process.terminate()
            encrypted_process.wait()


def backup(gateway, postgres, destination, recipient, signer, spool_dest, keys_dest, ledger_dest):
    if not re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", recipient) or not re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", signer):
        raise BackupError("備份加密金鑰識別值不正確")
    command(["gpg", "--batch", "--no-auto-key-retrieve", "--list-keys", recipient])
    command(["gpg", "--batch", "--no-auto-key-retrieve", "--list-secret-keys", signer])
    gateway_info, mounts = inspect_mounts(gateway, [spool_dest, keys_dest, ledger_dest])
    db_user, db_name = database_target(gateway_info, gateway, postgres)
    spool, keys, ledger = mounts
    spool_lock_mount = mount_spec(spool, "/capture", False)
    spool_read_mount = mount_spec(spool, "/data", True)
    keys_read_mount = mount_spec(keys, "/data", True)
    ledger_read_mount = mount_spec(ledger, "/data", True)
    output_path = destination.absolute()
    if "," in str(output_path):
        raise BackupError("備份目的地不正確")
    for mount in mounts:
        source = mount.get("Source")
        if source and os.path.commonpath([str(output_path.resolve()), str(Path(source).resolve())]) == str(Path(source).resolve()):
            raise BackupError("備份目的地不能在來源資料夾內")
    destination.mkdir(mode=0o700, parents=False, exist_ok=False)
    os.chmod(destination, 0o700)
    lock = subprocess.Popen(
        ["docker", "run", "--rm", "-i", "--pull=never", "--mount", spool_lock_mount,
         "--mount", f"type=bind,source={output_path},target=/backup",
         "--entrypoint", "sh", IMAGE, "-c",
         "test -f /capture/.capture-backup.lock && flock -n -x /capture/.capture-backup.lock sh -c 'echo READY; read release || exit 1; test \"$release\" = commit || exit 1; umask 077; printf \"ok\\n\" > /backup/.complete; chown " + str(os.getuid()) + ":" + str(os.getgid()) + " /backup/.complete'"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    )
    try:
        ready, _, _ = select.select([lock.stdout], [], [], 10)
        if not ready or lock.stdout.readline().strip() != b"READY" or lock.poll() is not None:
            raise BackupError("仍有保存作業進行中，無法開始備份")
        sources = {
            "gateway.dump.gpg": ["docker", "exec", postgres, "pg_dump", "-U", db_user, "-d", db_name, "-Fc"],
            "ledger-spool.tar.gpg": ["docker", "run", "--rm", "--pull=never", "--mount", ledger_read_mount, "--entrypoint", "tar", IMAGE, "-C", "/data", "-cf", "-", "."],
            "capture-spool.tar.gpg": ["docker", "run", "--rm", "--pull=never", "--mount", spool_read_mount, "--entrypoint", "tar", IMAGE, "-C", "/data", "-cf", "-", "."],
            "capture-keys.tar.gpg": ["docker", "run", "--rm", "--pull=never", "--mount", keys_read_mount, "--entrypoint", "tar", IMAGE, "-C", "/data", "-cf", "-", "."],
        }
        hashes = {}
        for name, producer in sources.items():
            if lock.poll() is not None:
                raise BackupError("備份協調鎖已中斷")
            output = destination / name
            encrypt_stream(producer, output, recipient, lock, b"PGDMP" if name == "gateway.dump.gpg" else None)
            digest = hashlib.sha256()
            with output.open("rb") as source:
                for block in iter(lambda: source.read(1024 * 1024), b""):
                    digest.update(block)
            hashes[name] = digest.hexdigest()
        if lock.poll() is not None:
            raise BackupError("備份協調鎖已中斷")
        manifest = {"version": 2, "created_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                    "gateway_id": gateway_info["Id"], "recipient_fingerprint": recipient.upper(),
                    "signer_fingerprint": signer.upper(), "sha256": hashes}
        manifest_path = destination / "manifest.json"
        with manifest_path.open("x", encoding="utf-8") as output:
            os.chmod(manifest_path, 0o600)
            json.dump(manifest, output, sort_keys=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        signature_path = destination / "manifest.sig"
        with signature_path.open("xb") as signature:
            os.chmod(signature_path, 0o600)
            signed = subprocess.run(["gpg", "--batch", "--no-auto-key-retrieve", "--local-user", signer,
                                     "--detach-sign", "--output", "-", str(manifest_path)],
                                    stdout=signature, stderr=subprocess.DEVNULL, timeout=60, check=False)
            if signed.returncode:
                raise BackupError("備份簽章失敗")
            signature.flush()
            os.fsync(signature.fileno())
        lock.stdin.write(b"commit\n")
        lock.stdin.flush()
        lock.stdin.close()
        if lock.wait(timeout=10) != 0 or (destination / ".complete").read_bytes() != b"ok\n":
            raise BackupError("備份協調鎖已中斷")
        directory_fd = os.open(destination, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        if lock.stdin:
            try:
                lock.stdin.close()
            except BrokenPipeError:
                pass
        try:
            lock.wait(timeout=10)
        except subprocess.TimeoutExpired:
            lock.terminate()
            lock.wait(timeout=10)
        if lock.stdout:
            lock.stdout.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--gateway", required=True)
    parser.add_argument("--postgres", required=True)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--recipient", required=True)
    parser.add_argument("--signer", required=True)
    parser.add_argument("--spool-destination", default="/var/lib/agent-pool-gateway/capture-spool")
    parser.add_argument("--keys-destination", default="/var/lib/agent-pool-gateway/capture-keys")
    parser.add_argument("--ledger-destination", default="/var/lib/agent-pool-gateway/ledger-spool")
    args = parser.parse_args()
    try:
        backup(args.gateway, args.postgres, args.destination, args.recipient, args.signer,
               args.spool_destination, args.keys_destination, args.ledger_destination)
    except (BackupError, OSError, subprocess.SubprocessError):
        print("備份失敗；原資料未變更，未完成的備份不可使用。", file=sys.stderr)
        return 1
    print(f"加密備份已建立：{args.destination}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
