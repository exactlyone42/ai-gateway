import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys


FILES = {"gateway.dump.gpg", "ledger-spool.tar.gpg", "capture-spool.tar.gpg", "capture-keys.tar.gpg"}
IMAGE = "postgres:17-alpine"


class VerifyError(Exception):
    pass


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


def verify_stream(file, archive_type):
    decrypt = subprocess.Popen(["gpg", "--batch", "--no-auto-key-retrieve", "--decrypt"], stdin=file,
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
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
    finally:
        if decrypt.stdout:
            decrypt.stdout.close()
        if decrypt.poll() is None:
            decrypt.terminate()
            decrypt.wait(timeout=10)


def verify_signature(directory, raw, signer):
    descriptor = os.memfd_create("capture-backup-manifest", os.MFD_CLOEXEC)
    with os.fdopen(descriptor, "w+b") as manifest, open_regular(directory / "manifest.sig") as signature:
        manifest.write(raw)
        manifest.flush()
        manifest.seek(0)
        result = subprocess.run(
            ["gpg", "--batch", "--no-auto-key-retrieve", "--status-fd", "1", "--verify",
             f"/proc/self/fd/{signature.fileno()}", f"/proc/self/fd/{manifest.fileno()}"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, pass_fds=(signature.fileno(), manifest.fileno()),
            timeout=60, check=False,
        )
        lines = [line.split() for line in result.stdout.decode("ascii").splitlines() if line.startswith("[GNUPG:] VALIDSIG ")]
        if result.returncode or len(lines) != 1 or signer.upper() not in (lines[0][2].upper(), lines[0][-1].upper()):
            raise VerifyError("備份簽章不正確")


def verify(directory, signer):
    if not re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", signer):
        raise VerifyError("簽署金鑰識別值不正確")
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
    if not isinstance(manifest, dict) or manifest.get("version") != 2 or manifest.get("signer_fingerprint") != signer.upper():
        raise VerifyError("備份版本不正確")
    hashes = manifest.get("sha256")
    if not isinstance(hashes, dict) or set(hashes) != FILES:
        raise VerifyError("備份清單不完整")
    for name in sorted(FILES):
        expected = hashes[name]
        if not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected):
            raise VerifyError("備份檢查碼不正確")
        with open_regular(directory / name) as file:
            if hash_file(file) != expected:
                raise VerifyError("備份檢查碼不一致")
            verify_stream(file, "database" if name == "gateway.dump.gpg" else "tar")
            if hash_file(file) != expected:
                raise VerifyError("驗證期間備份內容已改變")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path)
    parser.add_argument("--signer", required=True)
    args = parser.parse_args()
    try:
        verify(args.directory, args.signer)
    except (VerifyError, OSError, subprocess.SubprocessError):
        print("備份驗證失敗；不可使用此備份還原。", file=sys.stderr)
        return 1
    print("備份已通過檢查碼、解密及檔案格式驗證。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
