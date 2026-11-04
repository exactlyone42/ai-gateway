import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import subprocess
import sys

from verify_capture_backup import FILES, VerifyError, open_regular, receipt_payload, valid_sizes, verify_signature
from capture_postgres import valid_database_identity


def check(root, signer, recipient, gateway_id, postgres_id, database_system_id, database_oid, storage_id, max_age_hours):
    if not re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", signer) or not re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", recipient) or not re.fullmatch(r"[0-9a-f]{64}", gateway_id) or not re.fullmatch(r"[0-9a-f]{64}", postgres_id) or not valid_database_identity(database_system_id, database_oid) or not re.fullmatch(r"[0-9a-f]{64}", storage_id) or max_age_hours < 0:
        raise VerifyError("監控設定不正確")
    candidates = []
    for entry in root.iterdir():
        if re.fullmatch(r"[0-9]{8}T[0-9]{6}Z", entry.name):
            candidates.append(entry)
            if len(candidates) > 10000:
                raise VerifyError("備份目錄過多")
    if not candidates:
        raise VerifyError("沒有加密備份")
    directory = max(candidates, key=lambda item: item.name)
    if directory.is_symlink() or not directory.is_dir():
        raise VerifyError("備份目錄不正確")
    with open_regular(directory / ".complete") as marker:
        if marker.read(4) != b"ok\n":
            raise VerifyError("備份未完成")
    with open_regular(directory / "manifest.json") as file:
        raw = file.read(65537)
    if len(raw) > 65536:
        raise VerifyError("備份清單過大")
    verify_signature(directory, raw, signer)
    manifest = json.loads(raw)
    if not isinstance(manifest, dict) or type(manifest.get("version")) is not int or manifest.get("version") != 7 or manifest.get("signer_fingerprint") != signer.upper():
        raise VerifyError("備份清單不正確")
    if manifest.get("recipient_fingerprint") != recipient.upper():
        raise VerifyError("備份收件金鑰與預期不符")
    if manifest.get("gateway_id") != gateway_id:
        raise VerifyError("備份來源 Gateway 與預期不符")
    if manifest.get("postgres_id") != postgres_id:
        raise VerifyError("備份資料庫容器與預期不符")
    if manifest.get("database_system_id") != database_system_id or manifest.get("database_oid") != database_oid:
        raise VerifyError("備份資料庫身分與預期不符")
    if manifest.get("storage_identity") != storage_id:
        raise VerifyError("備份儲存來源與預期不符")
    if not isinstance(manifest.get("sha256"), dict) or set(manifest["sha256"]) != FILES:
        raise VerifyError("備份清單不完整")
    if not valid_sizes(manifest.get("size")):
        raise VerifyError("備份大小清單不正確")
    verify_signature(directory, receipt_payload(manifest, recipient), signer, "verified.sig")
    for name in FILES:
        with open_regular(directory / name) as file:
            if os.fstat(file.fileno()).st_size != manifest["size"][name]:
                raise VerifyError("備份大小不一致")
    created = datetime.fromisoformat(manifest["created_utc"])
    if created.tzinfo is None:
        raise VerifyError("備份時間沒有時區")
    age = (datetime.now(timezone.utc) - created).total_seconds()
    if age < 0 or age > max_age_hours * 3600:
        raise VerifyError("備份時間不在允許範圍")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path)
    parser.add_argument("--signer", required=True)
    parser.add_argument("--recipient", required=True)
    parser.add_argument("--gateway-id", required=True)
    parser.add_argument("--postgres-id", required=True)
    parser.add_argument("--database-system-id", required=True)
    parser.add_argument("--database-oid", required=True)
    parser.add_argument("--storage-id", required=True)
    parser.add_argument("--max-age-hours", type=int, required=True)
    args = parser.parse_args()
    try:
        check(args.root, args.signer, args.recipient, args.gateway_id, args.postgres_id, args.database_system_id, args.database_oid, args.storage_id, args.max_age_hours)
    except (VerifyError, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        print("加密備份新鮮度檢查失敗。", file=sys.stderr)
        return 1
    print("最近的加密備份已完成並曾通過驗證；此檢查不重新驗證備份內容。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
