import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import uuid


FILES = {"gateway.dump.gpg", "ledger-spool.tar.gpg", "capture-spool.tar.gpg", "capture-keys.tar.gpg"}
LEDGER_FILES = {"gateway.dump.gpg", "ledger-spool.tar.gpg"}
IMAGE = "postgres:17-alpine"


class VerifyError(Exception):
    pass


def valid_position(position):
    if not isinstance(position, dict) or set(position) != {"install", "sequence", "tip"}:
        return False
    install = position["install"]
    try:
        if not isinstance(install, str) or str(uuid.UUID(install)) != install:
            return False
    except ValueError:
        return False
    sequence = position["sequence"]
    tip = position["tip"]
    return type(sequence) is int and 0 <= sequence <= 2**63 - 1 and isinstance(tip, list) and len(tip) == 32 and all(type(item) is int and 0 <= item <= 255 for item in tip)


def open_regular(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    metadata = os.fstat(descriptor)
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_mode & 0o077:
        os.close(descriptor)
        raise VerifyError("備份檔案權限或類型不正確")
    return os.fdopen(descriptor, "rb")


def hash_file(file):
    file.seek(0)
    digest = hashlib.sha256()
    for block in iter(lambda: file.read(1024 * 1024), b""):
        digest.update(block)
    file.seek(0)
    return digest.hexdigest()


def verify_stream(file, archive_type, recipient=None):
    status_file = os.fdopen(os.memfd_create("capture-decryption-status", os.MFD_CLOEXEC), "w+b")
    try:
        decrypt = subprocess.Popen(["gpg", "--batch", "--no-auto-key-retrieve", "--status-fd", str(status_file.fileno()), "--decrypt"], stdin=file,
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, pass_fds=(status_file.fileno(),))
    except BaseException:
        status_file.close()
        raise
    if archive_type == "database":
        check = ["docker", "run", "--rm", "-i", "--pull=never", "--network", "none", "--entrypoint", "pg_restore", IMAGE, "--list"]
    else:
        check = ["docker", "run", "--rm", "-i", "--pull=never", "--network", "none", "--entrypoint", "tar", IMAGE, "-tf", "-"]
    try:
        result = subprocess.run(check, stdin=decrypt.stdout, stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL, timeout=3600, check=False)
        decrypt.stdout.close()
        status = decrypt.wait(timeout=10)
        if result.returncode or status:
            raise VerifyError("備份無法解密或格式不正確")
        if recipient is not None:
            status_file.seek(0)
            output = status_file.read(65537)
            if len(output) > 65536:
                raise VerifyError("解密狀態過大")
            keys = [line.split() for line in output.splitlines() if line.startswith(b"[GNUPG:] DECRYPTION_KEY ")]
            expected = recipient.upper().encode("ascii")
            if not any(len(line) >= 4 and expected in (line[2].upper(), line[3].upper()) for line in keys):
                raise VerifyError("實際解密金鑰與預期不符")
    finally:
        if decrypt.stdout:
            decrypt.stdout.close()
        if decrypt.poll() is None:
            decrypt.terminate()
            decrypt.wait(timeout=10)
        status_file.close()


def verify_signature(directory, raw, signer, signature_name="manifest.sig"):
    descriptor = os.memfd_create("capture-backup-manifest", os.MFD_CLOEXEC)
    with os.fdopen(descriptor, "w+b") as manifest, open_regular(directory / signature_name) as signature:
        manifest.write(raw)
        manifest.flush()
        manifest.seek(0)
        result = subprocess.run(
            ["gpg", "--batch", "--no-auto-key-retrieve", "--status-fd", "1", "--verify",
             f"/proc/self/fd/{signature.fileno()}", f"/proc/self/fd/{manifest.fileno()}"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, pass_fds=(signature.fileno(), manifest.fileno()),
            timeout=60, check=False,
        )
        statuses = [line.split() for line in result.stdout.decode("utf-8", errors="replace").splitlines() if line.startswith("[GNUPG:] ")]
        lines = [line for line in statuses if len(line) >= 3 and line[1] == "VALIDSIG"]
        good = [line for line in statuses if len(line) >= 3 and line[1] == "GOODSIG"]
        rejected = {"BADSIG", "ERRSIG", "EXPSIG", "EXPKEYSIG", "REVKEYSIG"}
        if result.returncode or any(len(line) >= 2 and line[1] in rejected for line in statuses) or len(good) != 1 or len(lines) != 1 or signer.upper() not in (lines[0][2].upper(), lines[0][-1].upper()):
            raise VerifyError("備份簽章不正確")


def valid_sizes(sizes, files=FILES):
    return isinstance(sizes, dict) and set(sizes) == files and all(type(value) is int and value > 0 for value in sizes.values())


def verify(directory, signer, recipient=None, inspect_legacy_v2=False, inspect_ledger_v8=False):
    if inspect_legacy_v2 and inspect_ledger_v8:
        raise VerifyError("備份檢查類型不正確")
    if not isinstance(signer, str) or not re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", signer):
        raise VerifyError("簽署金鑰識別值不正確")
    if recipient is not None and (not isinstance(recipient, str) or not re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", recipient)):
        raise VerifyError("收件金鑰識別值不正確")
    with open_regular(directory / ".complete") as marker:
        if marker.read(4) != b"ok\n":
            raise VerifyError("備份未完成")
    with open_regular(directory / "manifest.json") as file:
        raw = file.read(65537)
        if len(raw) > 65536:
            raise VerifyError("備份清單過大")
        verify_signature(directory, raw, signer)
        try:
            manifest = json.loads(raw)
        except (ValueError, UnicodeDecodeError) as error:
            raise VerifyError("備份清單不正確") from error
    allowed_versions = (2,) if inspect_legacy_v2 else (8,) if inspect_ledger_v8 else (3, 4, 5, 6, 7)
    if not isinstance(manifest, dict) or type(manifest.get("version")) is not int or manifest.get("version") not in allowed_versions or manifest.get("signer_fingerprint") != signer.upper() or (not inspect_legacy_v2 and not inspect_ledger_v8 and not valid_position(manifest.get("erasure_position"))):
        raise VerifyError("備份版本不正確")
    if inspect_ledger_v8 and (manifest.get("profile") != "ledger" or "erasure_position" in manifest):
        raise VerifyError("入帳備份類型不正確")
    if not inspect_ledger_v8 and "profile" in manifest and manifest["profile"] != "capture":
        raise VerifyError("備份類型不正確")
    if inspect_ledger_v8:
        for name in FILES - LEDGER_FILES:
            try:
                (directory / name).lstat()
            except FileNotFoundError:
                continue
            raise VerifyError("入帳備份含其他保存資料")
    if manifest["version"] in (5, 6, 7, 8) and (not isinstance(manifest.get("postgres_id"), str) or not re.fullmatch(r"[0-9a-f]{64}", manifest["postgres_id"])):
        raise VerifyError("備份資料庫識別不正確")
    if manifest["version"] in (6, 7, 8) and (not isinstance(manifest.get("database_system_id"), str) or not re.fullmatch(r"-?[0-9]{1,20}", manifest["database_system_id"]) or not -(2**63) <= int(manifest["database_system_id"]) < 2**63 or not isinstance(manifest.get("database_oid"), str) or not re.fullmatch(r"[0-9]{1,10}", manifest["database_oid"]) or int(manifest["database_oid"]) > 4294967295):
        raise VerifyError("備份資料庫身分不正確")
    if manifest["version"] in (7, 8) and (not isinstance(manifest.get("storage_identity"), str) or not re.fullmatch(r"[0-9a-f]{64}", manifest["storage_identity"])):
        raise VerifyError("備份儲存來源不正確")
    if recipient is not None and manifest.get("recipient_fingerprint") != recipient.upper():
        raise VerifyError("備份收件金鑰與預期不符")
    files = LEDGER_FILES if inspect_ledger_v8 else FILES
    hashes = manifest.get("sha256")
    if not isinstance(hashes, dict) or set(hashes) != files:
        raise VerifyError("備份清單不完整")
    sizes = manifest.get("size") if manifest["version"] in (4, 5, 6, 7, 8) else None
    if manifest["version"] in (4, 5, 6, 7, 8) and not valid_sizes(sizes, files):
        raise VerifyError("備份大小清單不正確")
    for name in sorted(files):
        expected = hashes[name]
        if not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected):
            raise VerifyError("備份檢查碼不正確")
        with open_regular(directory / name) as file:
            if sizes is not None and os.fstat(file.fileno()).st_size != sizes[name]:
                raise VerifyError("備份大小不一致")
            if hash_file(file) != expected:
                raise VerifyError("備份檢查碼不一致")
            verify_stream(file, "database" if name == "gateway.dump.gpg" else "tar", recipient)
            if hash_file(file) != expected:
                raise VerifyError("驗證期間備份內容已改變")
            if sizes is not None and os.fstat(file.fileno()).st_size != sizes[name]:
                raise VerifyError("驗證期間備份大小已改變")
    return manifest


def receipt_payload(manifest, recipient):
    return b"capture-backup-verified-v2\n" + recipient.upper().encode("ascii") + b"\n" + json.dumps(manifest, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("ascii")


def write_receipt(directory, signer, recipient):
    if not isinstance(recipient, str) or not re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", recipient):
        raise VerifyError("收件金鑰識別值不正確")
    directory_fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        manifest = verify(Path(f"/proc/self/fd/{directory_fd}"), signer, recipient)
        result = subprocess.run(["gpg", "--batch", "--no-auto-key-retrieve", "--local-user", signer, "--detach-sign", "--output", "-"], input=receipt_payload(manifest, recipient), stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=60, check=False)
        if result.returncode or not result.stdout:
            raise VerifyError("無法簽署備份驗證紀錄")
        temporary = ".verified-" + uuid.uuid4().hex
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=directory_fd)
        original = os.fstat(descriptor)
        try:
            with os.fdopen(descriptor, "wb") as receipt:
                receipt.write(result.stdout)
                receipt.flush()
                os.fsync(receipt.fileno())
            os.link(temporary, "verified.sig", src_dir_fd=directory_fd, dst_dir_fd=directory_fd, follow_symlinks=False)
            os.fsync(directory_fd)
        finally:
            current = os.stat(temporary, dir_fd=directory_fd, follow_symlinks=False)
            if (current.st_dev, current.st_ino) == (original.st_dev, original.st_ino):
                os.unlink(temporary, dir_fd=directory_fd)
    finally:
        os.close(directory_fd)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path)
    parser.add_argument("--signer", required=True)
    parser.add_argument("--recipient")
    parser.add_argument("--write-receipt", action="store_true")
    parser.add_argument("--inspect-legacy-v2", action="store_true")
    parser.add_argument("--inspect-ledger-v8", action="store_true")
    args = parser.parse_args()
    try:
        if (args.inspect_legacy_v2 and args.inspect_ledger_v8) or (args.write_receipt and (not args.recipient or args.inspect_legacy_v2 or args.inspect_ledger_v8)):
            raise VerifyError("產生驗證紀錄前必須核對收件金鑰")
        if args.write_receipt:
            write_receipt(args.directory, args.signer, args.recipient)
        else:
            verify(args.directory, args.signer, args.recipient, args.inspect_legacy_v2, args.inspect_ledger_v8)
    except (VerifyError, OSError, subprocess.SubprocessError):
        print("備份驗證失敗；不可使用此備份還原。", file=sys.stderr)
        return 1
    if args.inspect_legacy_v2:
        print("舊版備份已通過簽章、檢查碼、解密及格式檢查；缺少刪除紀錄，不能直接還原。")
    elif args.inspect_ledger_v8:
        print("候選檔案已通過簽章、檢查碼、解密及格式檢查；不確認資料庫內容，不能直接還原。")
    else:
        print("備份已通過檢查碼、解密及檔案格式驗證。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
