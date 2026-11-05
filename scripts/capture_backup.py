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
import uuid
from urllib.parse import parse_qsl, unquote, urlsplit
from capture_postgres import configured_port, database_identity, local_exec
from capture_storage_identity import StorageIdentityError, require_local_docker, storage_identity
from verify_capture_backup import valid_position


IMAGE = "postgres:17-alpine"
LOCK_WAIT_SECONDS = 10


class BackupError(Exception):
    pass


def command(args):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=15, check=False)
    if result.returncode:
        raise BackupError("無法檢查備份來源")
    return result.stdout


def mount_spec(mount, target, readonly):
    kind = mount.get("Type")
    source = mount.get("Name")
    if kind != "volume" or not isinstance(source, str) or not source or "," in source:
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


def validate_storage_environment(gateway, destinations):
    keys = ("CONTENT_CAPTURE_SPOOL_DIR", "CONTENT_CAPTURE_KEY_DIR", "LEDGER_SPOOL_DIR", "CONTENT_CAPTURE_ERASURE_DIR", "CONTENT_CAPTURE_WITNESS_DIR")
    environment = gateway["Config"]["Env"]
    for key, destination in zip(keys, destinations):
        values = [entry.split("=", 1)[1] for entry in environment if entry.startswith(key + "=")]
        if values != [destination]:
            raise BackupError("Gateway 保存路徑未明確對應掛載")


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
        if any(key.lower() in {"host", "hostaddr", "port", "user", "dbname", "service", "servicefile"} for key, _ in parse_qsl(parsed.query, keep_blank_values=True)):
            raise BackupError("資料庫來源含有覆寫連線目標的參數")
        target_port = configured_port(target)
        if parsed.port is None or parsed.port != target_port:
            raise BackupError("資料庫連接埠與 Gateway 設定不符")
        gateway_networks = gateway["NetworkSettings"]["Networks"]
        target_networks = target["NetworkSettings"]["Networks"]
        candidates = set()
        matched_networks = 0
        for network in set(gateway_networks).intersection(target_networks):
            details = target_networks[network]
            names = {name.lower() for name in details.get("Aliases") or []}
            names.add(details.get("IPAddress", "").lower())
            names.add(details.get("GlobalIPv6Address", "").lower())
            names.add(target.get("Name", "").lstrip("/").lower())
            if parsed.hostname in names:
                matched_networks += 1
                for address in (details.get("IPAddress", ""), details.get("GlobalIPv6Address", "")):
                    if address:
                        candidates.add(str(ipaddress.ip_address(address)))
        if matched_networks != 1 or not candidates:
            raise BackupError("指定的資料庫不是 Gateway 的資料庫")
        addresses = set()
        for lookup in ("ahosts", "hosts"):
            resolved = command(["docker", "exec", gateway_name, "getent", lookup, parsed.hostname]).decode("ascii")
            addresses.update(str(ipaddress.ip_address(line.split()[0])) for line in resolved.splitlines() if line.split())
        if not addresses or not addresses.issubset(candidates):
            raise BackupError("資料庫名稱解析不唯一或與指定容器不符")
        db_name = unquote(parsed.path.lstrip("/"))
        if not db_name or "=" in db_name or db_name.startswith(("postgres://", "postgresql://")):
            raise BackupError("資料庫名稱不正確")
        return unquote(parsed.username), db_name, target["Id"], target["State"]["StartedAt"], target_port
    except (KeyError, IndexError, TypeError, ValueError) as error:
        raise BackupError("無法核對資料庫來源") from error


def require_same_instance(identifier, started):
    details = json.loads(command(["docker", "inspect", identifier]))
    if len(details) != 1 or details[0]["Id"] != identifier or not details[0]["State"]["Running"] or details[0]["State"]["StartedAt"] != started:
        raise BackupError("備份期間服務已重新啟動")


def start_backup_source(producer):
    nonce = None
    if producer[:2] == ["docker", "run"]:
        if "--rm" not in producer or "--name" in producer:
            raise BackupError("備份容器設定不正確")
        nonce = uuid.uuid4().hex
        expiry = (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=1)).date().isoformat()
        producer = ["docker", "run", "--name", "gateway-backup-source-" + nonce, "--label", "gateway.backup_producer=" + nonce, "--label", "expire=" + expiry, *producer[2:]]
    return subprocess.Popen(producer, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL), nonce


def finish_backup_source(source, nonce):
    if source.stdout:
        source.stdout.close()
    if source.poll() is None:
        source.terminate()
        try:
            source.wait(timeout=5)
        except subprocess.TimeoutExpired:
            source.kill()
            source.wait(timeout=5)
    if nonce is None:
        return
    listed = subprocess.run(["docker", "ps", "-aq", "--no-trunc", "--filter", "label=gateway.backup_producer=" + nonce], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=15, check=False)
    if listed.returncode:
        raise BackupError("無法確認備份容器清理狀態")
    identifiers = listed.stdout.decode("ascii").splitlines()
    if len(identifiers) > 1 or any(not re.fullmatch(r"[0-9a-f]{64}", item) for item in identifiers):
        raise BackupError("備份容器識別不正確")
    if identifiers:
        removed = subprocess.run(["docker", "rm", "-f", "-v", identifiers[0]], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=False)
        if removed.returncode:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                remaining = subprocess.run(["docker", "ps", "-aq", "--no-trunc", "--filter", "label=gateway.backup_producer=" + nonce], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=15, check=False)
                if remaining.returncode == 0 and not remaining.stdout.strip():
                    break
                time.sleep(0.1)
            else:
                raise BackupError("備份容器清理失敗")


def encrypt_stream(producer, output, recipient, lock, prefix):
    source, nonce = start_backup_source(producer)
    encrypted_process = None
    plaintext_digest = hashlib.sha256()
    try:
        with output.open("xb") as encrypted:
            os.chmod(output, 0o600)
            encrypted_process = subprocess.Popen(
                ["gpg", "--batch", "--no-auto-key-retrieve", "--no-encrypt-to", "--yes", "--trust-model", "always", "--recipient", recipient, "--encrypt"],
                stdin=subprocess.PIPE, stdout=encrypted, stderr=subprocess.DEVNULL, bufsize=0,
            )
            os.set_blocking(encrypted_process.stdin.fileno(), False)
            deadline = time.monotonic() + 3600
            first = bytearray()
            validated = False

            def write_block(data):
                offset = 0
                while offset < len(data):
                    if lock.poll() is not None:
                        raise BackupError("備份協調鎖已中斷")
                    if encrypted_process.poll() is not None:
                        raise BackupError("備份加密失敗")
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise BackupError("備份加密逾時")
                    _, writable, _ = select.select([], [encrypted_process.stdin], [], min(5, remaining))
                    if writable:
                        try:
                            offset += os.write(encrypted_process.stdin.fileno(), memoryview(data)[offset:])
                        except BlockingIOError:
                            continue

            while True:
                if lock.poll() is not None:
                    raise BackupError("備份協調鎖已中斷")
                if encrypted_process.poll() is not None:
                    raise BackupError("備份加密失敗")
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise BackupError("備份來源逾時")
                ready, _, _ = select.select([source.stdout], [], [], min(5, remaining))
                if not ready:
                    continue
                block = os.read(source.stdout.fileno(), 65536)
                if not block:
                    break
                plaintext_digest.update(block)
                if not validated:
                    first.extend(block)
                    needed = len(prefix) if prefix else 512
                    if len(first) < needed:
                        continue
                    if prefix and not first.startswith(prefix):
                        raise BackupError("備份來源格式不正確")
                    write_block(first)
                    first[:] = bytes(len(first))
                    first.clear()
                    validated = True
                else:
                    write_block(block)
            if not validated:
                raise BackupError("備份來源為空或格式不正確")
            encrypted_process.stdin.close()
            while source.poll() is None or encrypted_process.poll() is None:
                if lock.poll() is not None:
                    raise BackupError("備份協調鎖已中斷")
                if time.monotonic() >= deadline:
                    raise BackupError("備份加密逾時")
                time.sleep(0.05)
            if encrypted_process.returncode or source.returncode:
                raise BackupError("備份加密失敗")
            encrypted.flush()
            os.fsync(encrypted.fileno())
            return plaintext_digest.hexdigest()
    finally:
        try:
            if encrypted_process and encrypted_process.stdin:
                try:
                    encrypted_process.stdin.close()
                except BrokenPipeError:
                    pass
            if encrypted_process and encrypted_process.poll() is None:
                encrypted_process.terminate()
                try:
                    encrypted_process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    encrypted_process.kill()
                    encrypted_process.wait(timeout=5)
        finally:
            finish_backup_source(source, nonce)


def backup(gateway, postgres, destination, recipient, signer, spool_dest, keys_dest, ledger_dest, erasure_dest, witness_dest, database_user=None):
    if database_user is not None and not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]{0,62}", database_user):
        raise BackupError("備份資料庫帳號名稱不正確")
    if not re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", recipient) or not re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", signer):
        raise BackupError("備份加密金鑰識別值不正確")
    require_local_docker()
    command(["gpg", "--batch", "--no-auto-key-retrieve", "--list-keys", recipient])
    command(["gpg", "--batch", "--no-auto-key-retrieve", "--list-secret-keys", signer])
    gateway_info, mounts = inspect_mounts(gateway, [spool_dest, keys_dest, ledger_dest, erasure_dest, witness_dest])
    validate_storage_environment(gateway_info, [spool_dest, keys_dest, ledger_dest, erasure_dest, witness_dest])
    gateway_id = gateway_info["Id"]
    db_user, db_name, postgres_id, postgres_started, postgres_port = database_target(gateway_info, gateway_id, postgres)
    if database_user is not None:
        db_user = database_user
    db_identity = database_identity(postgres_id, postgres_port, db_user, db_name)
    gateway_started = gateway_info["State"]["StartedAt"]
    spool, keys, ledger, erasure, witness = mounts
    for mount in (erasure, witness):
        mount_spec(mount, "/check", True)
        name = mount.get("Name")
        if mount.get("Type") == "volume" and isinstance(name, str) and re.fullmatch(r"[0-9a-f]{64}", name):
            raise BackupError("刪除紀錄和見證不能使用匿名磁碟卷")
    spool_lock_mount = mount_spec(spool, "/capture", False)
    spool_read_mount = mount_spec(spool, "/data", True)
    keys_read_mount = mount_spec(keys, "/data", True)
    keys_guard_mount = mount_spec(keys, "/keys", True)
    ledger_read_mount = mount_spec(ledger, "/data", True)
    ledger_lock_mount = mount_spec(ledger, "/ledger", False)
    output_path = destination.absolute()
    if "," in str(output_path):
        raise BackupError("備份目的地不正確")
    for mount in mounts:
        source = mount.get("Source")
        if source and os.path.commonpath([str(output_path.resolve()), str(Path(source).resolve())]) == str(Path(source).resolve()):
            raise BackupError("備份目的地不能在來源資料夾內")
    try:
        destination.mkdir(mode=0o700, parents=False, exist_ok=False)
    except FileExistsError as error:
        raise BackupError("備份目的地已存在") from error
    os.chmod(destination, 0o700)
    deadline = time.monotonic() + LOCK_WAIT_SECONDS
    while True:
        lock = subprocess.Popen(
            ["docker", "run", "--rm", "-i", "--pull=never", "--mount", spool_lock_mount, "--mount", ledger_lock_mount, "--mount", keys_guard_mount,
             "--mount", f"type=bind,source={output_path},target=/backup",
             "--entrypoint", "sh", IMAGE, "-c",
             "test -f /capture/.capture-backup.lock && test -f /ledger/.capture-backup.lock && flock -n -x /capture/.capture-backup.lock flock -n -x /ledger/.capture-backup.lock sh -c 'matches=$(find /keys -maxdepth 1 -name \".initializing*\" -print -quit) || exit 1; test -z \"$matches\" || exit 1; echo READY; read release || exit 1; test \"$release\" = commit || exit 1; umask 077; printf \"ok\\n\" > /backup/.complete; chown " + str(os.getuid()) + ":" + str(os.getgid()) + " /backup/.complete'"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        )
        ready, _, _ = select.select([lock.stdout], [], [], 10)
        if ready and lock.stdout.readline().strip() == b"READY" and lock.poll() is None:
            break
        lock.kill()
        lock.wait()
        if time.monotonic() >= deadline:
            raise BackupError("仍有保存作業進行中，無法開始備份")
        time.sleep(0.2)
    try:
        storage = storage_identity(mounts)
        position = json.loads(command(["docker", "exec", gateway_id, "/proc/1/exe", "--inspect-capture-erasure"]))
        if not valid_position(position):
            raise BackupError("刪除紀錄位置不正確")
        sources = {
            "gateway.dump.gpg": local_exec(postgres_id, postgres_port, "pg_dump") + ["-U", db_user, "-d", db_name, "-Fc"],
            "ledger-spool.tar.gpg": ["docker", "run", "--rm", "--pull=never", "--mount", ledger_read_mount, "--entrypoint", "tar", IMAGE, "-C", "/data", "-cf", "-", "."],
            "capture-spool.tar.gpg": ["docker", "run", "--rm", "--pull=never", "--mount", spool_read_mount, "--entrypoint", "tar", IMAGE, "-C", "/data", "-cf", "-", "."],
            "capture-keys.tar.gpg": ["docker", "run", "--rm", "--pull=never", "--mount", keys_read_mount, "--entrypoint", "tar", IMAGE, "-C", "/data", "-cf", "-", "."],
        }
        hashes = {}
        sizes = {}
        for name, producer in sources.items():
            if lock.poll() is not None:
                raise BackupError("備份協調鎖已中斷")
            output = destination / name
            encrypt_stream(producer, output, recipient, lock, b"PGDMP" if name == "gateway.dump.gpg" else None)
            digest = hashlib.sha256()
            size = 0
            with output.open("rb") as source:
                for block in iter(lambda: source.read(1024 * 1024), b""):
                    digest.update(block)
                    size += len(block)
                if size == 0 or os.fstat(source.fileno()).st_size != size:
                    raise BackupError("備份檔案大小不正確")
            hashes[name] = digest.hexdigest()
            sizes[name] = size
        if lock.poll() is not None:
            raise BackupError("備份協調鎖已中斷")
        final_position = json.loads(command(["docker", "exec", gateway_id, "/proc/1/exe", "--inspect-capture-erasure"]))
        if not valid_position(final_position) or final_position != position:
            raise BackupError("備份期間刪除紀錄已改變")
        require_same_instance(gateway_id, gateway_started)
        require_same_instance(postgres_id, postgres_started)
        if database_identity(postgres_id, postgres_port, db_user, db_name) != db_identity:
            raise BackupError("備份期間資料庫身分已改變")
        current_gateway, current_mounts = inspect_mounts(gateway_id, [spool_dest, keys_dest, ledger_dest, erasure_dest, witness_dest])
        validate_storage_environment(current_gateway, [spool_dest, keys_dest, ledger_dest, erasure_dest, witness_dest])
        if storage_identity(current_mounts) != storage:
            raise BackupError("備份期間儲存來源已改變")
        if lock.poll() is not None:
            raise BackupError("備份協調鎖已中斷")
        manifest = {"version": 7, "created_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                    "gateway_id": gateway_info["Id"], "postgres_id": postgres_id, "database_system_id": db_identity[0], "database_oid": db_identity[1], "recipient_fingerprint": recipient.upper(),
                    "storage_identity": storage, "signer_fingerprint": signer.upper(), "erasure_position": position, "sha256": hashes, "size": sizes}
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
    parser.add_argument("--database-user", default=os.environ.get("CAPTURE_BACKUP_DATABASE_USER"))
    parser.add_argument("--spool-destination", default="/var/lib/agent-pool-gateway/capture-spool")
    parser.add_argument("--keys-destination", default="/var/lib/agent-pool-gateway/capture-keys")
    parser.add_argument("--ledger-destination", default="/var/lib/agent-pool-gateway/ledger-spool")
    parser.add_argument("--erasure-destination", default="/var/lib/agent-pool-gateway/capture-erasure")
    parser.add_argument("--witness-destination", default="/var/lib/agent-pool-gateway/capture-witness")
    args = parser.parse_args()
    try:
        backup(args.gateway, args.postgres, args.destination, args.recipient, args.signer,
               args.spool_destination, args.keys_destination, args.ledger_destination,
               args.erasure_destination, args.witness_destination, args.database_user)
    except BackupError as error:
        print(f"備份失敗：{error}；原資料未變更，未完成的備份不可使用。", file=sys.stderr)
        return 1
    except (StorageIdentityError, OSError, subprocess.SubprocessError, ValueError, KeyError, IndexError, TypeError):
        print("備份失敗；原資料未變更，未完成的備份不可使用。", file=sys.stderr)
        return 1
    print(f"加密備份已建立：{args.destination}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
