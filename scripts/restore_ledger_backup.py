import argparse
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tarfile
import tempfile

from capture_postgres import local_exec
from restore_capture_backup import RestoreError, check_dump_expansion, decrypt_to, output, preflight, require_same_target, stage_backup, tar_restore
from stage_legacy_backup import NAMES
from verify_capture_backup import FILES, LEDGER_FILES, VerifyError, open_regular, verify


def check_ledger_archive(path, maximum):
    total = 0
    seen = set()
    with open_regular(path) as source:
        decrypt = subprocess.Popen(["gpg", "--batch", "--no-auto-key-retrieve", "--decrypt"], stdin=source, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        try:
            with tarfile.open(fileobj=decrypt.stdout, mode="r|*") as archive:
                for item in archive:
                    name = str(PurePosixPath(item.name))
                    if name in seen or len(seen) >= 100000:
                        raise RestoreError("入帳備份包含重複或過多檔案")
                    seen.add(name)
                    if item.isdir() and name == ".":
                        continue
                    if not item.isfile() or len(PurePosixPath(item.name).parts) != 1 or not NAMES.fullmatch(name):
                        raise RestoreError("入帳備份包含不安全的檔案")
                    if item.size < 0 or item.size > maximum - total:
                        raise RestoreError("入帳備份超過還原上限")
                    if name.startswith(".") and item.size != 0:
                        raise RestoreError("入帳鎖檔不正確")
                    total += item.size
            decrypt.stdout.close()
            if decrypt.wait(timeout=10):
                raise RestoreError("入帳備份無法解密")
        finally:
            if decrypt.stdout:
                decrypt.stdout.close()
            if decrypt.poll() is None:
                decrypt.terminate()
                decrypt.wait(timeout=10)


def restore(args):
    if args.max_bytes <= 0 or args.uid < 1 or args.gid < 1:
        raise RestoreError("還原限制不正確")
    for name in FILES - LEDGER_FILES:
        if (args.backup / name).exists() or (args.backup / name).is_symlink():
            raise RestoreError("入帳備份含其他保存資料")
    with tempfile.TemporaryDirectory(prefix="ledger-restore-", dir=args.staging) as directory:
        staged = Path(directory)
        stage_backup(args.backup, staged, args.max_bytes, LEDGER_FILES)
        verify(staged, args.signer, args.recipient, inspect_ledger_v8=True)
        check_ledger_archive(staged / "ledger-spool.tar.gpg", args.max_bytes)
        target, started, port = preflight(args.postgres, args.db_user, args.db_name, [args.ledger], 1)
        check_dump_expansion(staged / "gateway.dump.gpg", target, args.max_bytes)
        verify(staged, args.signer, args.recipient, inspect_ledger_v8=True)
        require_same_target(target, started)
        decrypt_to(staged / "gateway.dump.gpg", local_exec(target, port, "pg_restore") + ["-U", args.db_user, "-d", args.db_name, "--exit-on-error", "--single-transaction"])
        query = (Path(__file__).resolve().parent.parent / "queries/capture_backup_schema_present.sql").read_bytes()
        result = output(local_exec(target, port, "psql") + ["-U", args.db_user, "-d", args.db_name, "-Atq", "-v", "ON_ERROR_STOP=1"], query).strip()
        if result != b"f":
            raise RestoreError("資料庫含全文保存資料或狀態不明")
        require_same_target(target, started)
        tar_restore(staged / "ledger-spool.tar.gpg", args.ledger, args.uid, args.gid)
        require_same_target(target, started)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--backup", type=Path, required=True)
    parser.add_argument("--signer", required=True)
    parser.add_argument("--recipient", required=True)
    parser.add_argument("--postgres", required=True)
    parser.add_argument("--ledger", required=True)
    parser.add_argument("--db-user", default="gateway")
    parser.add_argument("--db-name", default="gateway")
    parser.add_argument("--uid", type=int, default=10001)
    parser.add_argument("--gid", type=int, default=999)
    parser.add_argument("--max-bytes", type=int, default=16 * 1024 * 1024 * 1024)
    parser.add_argument("--staging", type=Path)
    args = parser.parse_args()
    try:
        restore(args)
    except (RestoreError, VerifyError, OSError, subprocess.SubprocessError, ValueError, KeyError, IndexError, tarfile.TarError):
        print("還原失敗；原備份未變更，目標可能不完整，請勿啟動服務。", file=sys.stderr)
        return 1
    print("已還原至隔離目標；啟動前須另行確認設定與資料。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
