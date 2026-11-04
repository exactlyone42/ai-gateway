import datetime
import hashlib
import json
import os
import re
import subprocess

from capture_backup import BackupError, encrypt_stream
from verify_capture_backup import LEDGER_FILES, VerifyError, hash_file, open_regular, verify_signature


def write_bundle(destination, sources, recipient, signer, identity, lock, final_check=None, expected_hashes=None, precreated=False):
    if set(sources) != LEDGER_FILES or not all(re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", value) for value in (recipient, signer)):
        raise BackupError("入帳備份設定不正確")
    required = {"gateway_id", "postgres_id", "database_system_id", "database_oid", "storage_identity"}
    if set(identity) != required or lock.poll() is not None:
        raise BackupError("入帳備份來源或協調狀態不正確")
    if precreated:
        if not destination.is_dir() or any(destination.iterdir()):
            raise BackupError("入帳備份目的地不為空")
    else:
        destination.mkdir(mode=0o700)
    hashes, sizes = {}, {}
    for name in sorted(LEDGER_FILES):
        if lock.poll() is not None:
            raise BackupError("備份協調鎖已中斷")
        path = destination / name
        plaintext_hash = encrypt_stream(sources[name], path, recipient, lock, b"PGDMP" if name == "gateway.dump.gpg" else None)
        if expected_hashes is not None and name in expected_hashes and plaintext_hash != expected_hashes[name]:
            raise BackupError("入帳備份內容與來源不一致")
        digest = hashlib.sha256()
        size = 0
        with path.open("rb") as item:
            for block in iter(lambda: item.read(1024 * 1024), b""):
                size += len(block)
                digest.update(block)
            if not size or os.fstat(item.fileno()).st_size != size:
                raise BackupError("入帳備份大小不正確")
        hashes[name] = digest.hexdigest()
        sizes[name] = size
    if lock.poll() is not None:
        raise BackupError("備份協調鎖已中斷")
    manifest = {"version": 8, "profile": "ledger", "created_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(), **identity, "recipient_fingerprint": recipient.upper(), "signer_fingerprint": signer.upper(), "sha256": hashes, "size": sizes}
    path = destination / "manifest.json"
    manifest_bytes = (json.dumps(manifest, sort_keys=True) + "\n").encode("utf-8")
    with path.open("xb") as item:
        os.chmod(path, 0o600)
        item.write(manifest_bytes)
        item.flush()
        os.fsync(item.fileno())
    signature = destination / "manifest.sig"
    with signature.open("xb") as item, path.open("rb") as manifest_file:
        os.chmod(signature, 0o600)
        result = subprocess.run(["gpg", "--batch", "--no-auto-key-retrieve", "--local-user", signer, "--detach-sign", "--output", "-", f"/proc/self/fd/{manifest_file.fileno()}"], stdout=item, stderr=subprocess.DEVNULL, timeout=60, check=False, pass_fds=(manifest_file.fileno(),))
        if result.returncode or lock.poll() is not None:
            raise BackupError("入帳備份簽章或協調失敗")
        item.flush()
        os.fsync(item.fileno())
    if final_check is not None:
        final_check()
    if lock.poll() is not None:
        raise BackupError("備份協調鎖已中斷")
    try:
        with open_regular(path) as item:
            if item.read(len(manifest_bytes) + 1) != manifest_bytes:
                raise BackupError("完成前入帳備份清單已改變")
        verify_signature(destination, manifest_bytes, signer)
        for name in sorted(LEDGER_FILES):
            with open_regular(destination / name) as item:
                if os.fstat(item.fileno()).st_size != sizes[name] or hash_file(item) != hashes[name]:
                    raise BackupError("完成前入帳備份內容已改變")
    except VerifyError as error:
        raise BackupError("完成前入帳備份檔案或簽章不正確") from error
    try:
        lock.stdin.write(b"commit\n")
        lock.stdin.flush()
        lock.stdin.close()
        if lock.wait(timeout=10) != 0 or (destination / ".complete").read_bytes() != b"ok\n":
            raise BackupError("入帳備份協調程序未確認完成")
    except (BrokenPipeError, OSError, subprocess.SubprocessError) as error:
        raise BackupError("入帳備份協調程序未確認完成") from error
    descriptor = os.open(destination, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
