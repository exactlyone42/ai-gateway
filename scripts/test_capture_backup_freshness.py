from datetime import datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

from scripts.verify_capture_backup import FILES, receipt_payload, valid_sizes


ROOT = Path(__file__).resolve().parent.parent


class CaptureBackupFreshnessTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / "gpg"
        self.home.mkdir(mode=0o700)
        self.backups = self.root / "backups"
        self.backups.mkdir()
        self.env = os.environ.copy()
        self.env.update(COMPOSE_PROJECT="apg", GNUPGHOME=str(self.home), BACKUP_ROOT=str(self.backups), HEALTHCHECK_STATUS_FILE=str(self.root / "status"), ALERT_LOG=str(self.root / "alerts"))
        self.env.pop("ALERT_WEBHOOK_URL", None)
        self.env.pop("TEST_SCHEMA", None)
        self.env.pop("TEST_VOLUME_CREATED", None)
        self.env.pop("DOCKER_HOST", None)
        self.env.pop("DOCKER_CONTEXT", None)
        subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Freshness Test <freshness@example.invalid>", "rsa2048", "sign", "0"], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        listing = subprocess.run(["gpg", "--batch", "--with-colons", "--list-secret-keys"], env=self.env, capture_output=True, text=True, check=True).stdout
        self.signer = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
        self.env["CAPTURE_BACKUP_SIGNER"] = self.signer
        self.recipient = "A" * 40
        self.env["CAPTURE_BACKUP_RECIPIENT"] = self.recipient
        self.env["CAPTURE_BACKUP_GATEWAY"] = "fixture-gateway"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        docker = '''#!/usr/bin/python3
import json, os, sys
args = sys.argv[1:]
gateway_id, database_id = "a" * 64, os.environ.get("TEST_POSTGRES_ID", "b" * 64)
started = "2026-11-03T00:00:00Z"
destinations = ['/var/lib/agent-pool-gateway/capture-spool', '/var/lib/agent-pool-gateway/capture-keys', '/var/lib/agent-pool-gateway/ledger-spool', '/var/lib/agent-pool-gateway/capture-erasure', '/var/lib/agent-pool-gateway/capture-witness']
names = ['spool', 'keys', 'ledger', 'erasure', 'witness']
keys = ['CONTENT_CAPTURE_SPOOL_DIR', 'CONTENT_CAPTURE_KEY_DIR', 'LEDGER_SPOOL_DIR', 'CONTENT_CAPTURE_ERASURE_DIR', 'CONTENT_CAPTURE_WITNESS_DIR']
if args[0] == "context" and args[1] == "show":
    print("fixture")
elif args[0] == "context" and args[1] == "inspect":
    print(json.dumps([] if os.environ.get("TEST_EMPTY_CONTEXT") else [{"Endpoints":{"docker":{"Host":"unix:///fixture.sock"}}}]))
elif args[0] == "volume" and args[1] == "inspect":
    print(json.dumps([{"Name":name,"Driver":"local","Scope":"local","CreatedAt":os.environ.get("TEST_VOLUME_CREATED", started),"Options":{}} for name in args[2:]]))
elif args[0] == "inspect" and args[1] in ("fixture-gateway", gateway_id):
    mounts = [{"Type":"volume","Name":name,"Destination":destination} for name, destination in zip(names, destinations)]
    env = ["DATABASE_URL=postgres://gateway:fixture@apg-postgres-1:5432/gateway_jev_local"] + [key + "=" + destination for key, destination in zip(keys, destinations)]
    print(json.dumps([{"Id":gateway_id,"State":{"Running":True,"StartedAt":started},"Mounts":mounts,"Config":{"Env":env},"NetworkSettings":{"Networks":{"fixture":{}}}}]))
elif args[0] == "inspect" and args[1] in ("apg-postgres-1", database_id):
    print(json.dumps([{"Id":database_id,"Name":"/apg-postgres-1","State":{"Running":True,"StartedAt":started},"Config":{"Env":[]},"NetworkSettings":{"Networks":{"fixture":{"Aliases":["apg-postgres-1"],"IPAddress":"172.18.0.2","GlobalIPv6Address":""}}}}]))
elif args[0] == "exec" and "getent" in args:
    print("172.18.0.2 STREAM apg-postgres-1")
elif args[0] == "exec" and "psql" in args and args[args.index("-d") + 1] == "gateway_jev_local":
    query = sys.stdin.read()
    if "pg_control_system" in query:
        print(os.environ.get("TEST_DATABASE_SYSTEM_ID", "1234") + "|" + os.environ.get("TEST_DATABASE_OID", "123"))
    else:
        print(os.environ.get("TEST_SCHEMA", "t"))
else:
    sys.exit(1)
'''
        for name, content in (("docker", docker), ("curl", "#!/bin/sh\nexit 0\n")):
            path = self.bin / name
            path.write_text(content)
            path.chmod(0o755)
        self.env["PATH"] = f"{self.bin}:{self.env['PATH']}"

    def sign(self, data, path):
        result = subprocess.run(["gpg", "--batch", "--local-user", self.signer, "--detach-sign", "--output", "-"], input=data, env=self.env, capture_output=True, check=True)
        path.write_bytes(result.stdout)
        path.chmod(0o600)

    def make_backup(self, name="20261102T000000Z", age=timedelta(), receipt=True):
        directory = self.backups / name
        directory.mkdir(mode=0o700)
        names = ("spool", "keys", "ledger", "erasure", "witness")
        destinations = tuple("/var/lib/agent-pool-gateway/" + name for name in ("capture-spool", "capture-keys", "ledger-spool", "capture-erasure", "capture-witness"))
        identities = [{"destination": destination, "type": "volume", "identity": {"Name": name, "Driver": "local", "Scope": "local", "CreatedAt": "2026-11-03T00:00:00Z"}} for name, destination in zip(names, destinations)]
        encoded = json.dumps(identities, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("ascii")
        storage = hashlib.sha256(b"capture-storage-v1\0" + encoded).hexdigest()
        manifest = {"version": 7, "created_utc": (datetime.now(timezone.utc) - age).isoformat(), "signer_fingerprint": self.signer, "recipient_fingerprint": self.recipient, "gateway_id": "a" * 64, "postgres_id": "b" * 64, "database_system_id": "1234", "database_oid": "123", "storage_identity": storage, "sha256": {name: "0" * 64 for name in FILES}, "size": {name: len(b"fixture") for name in FILES}}
        raw = json.dumps(manifest).encode()
        for filename, content in (("manifest.json", raw), (".complete", b"ok\n"), *((name, b"fixture") for name in FILES)):
            path = directory / filename
            path.write_bytes(content)
            path.chmod(0o600)
        self.sign(raw, directory / "manifest.sig")
        if receipt:
            self.sign(receipt_payload(manifest, self.recipient), directory / "verified.sig")
        return directory

    def health(self):
        return subprocess.run(["bash", "scripts/healthcheck.sh"], cwd=ROOT, env=self.env, capture_output=True, text=True)

    def test_signed_recent_backup_is_recognized(self):
        self.make_backup()
        result = self.health()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("不重新驗證備份內容", result.stdout)

    def test_old_receipt_does_not_prove_recipient_was_checked(self):
        directory = self.make_backup()
        manifest = json.loads((directory / "manifest.json").read_text())
        old_payload = b"capture-backup-verified-v1\n" + json.dumps(manifest, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("ascii")
        self.sign(old_payload, directory / "verified.sig")
        result = self.health()
        self.assertNotEqual(result.returncode, 0)

    def test_missing_or_wrong_recipient_is_rejected(self):
        self.make_backup()
        self.assertEqual(self.health().returncode, 0)
        self.env["CAPTURE_BACKUP_RECIPIENT"] = "B" * 40
        self.assertNotEqual(self.health().returncode, 0)
        self.env.pop("CAPTURE_BACKUP_RECIPIENT")
        self.assertNotEqual(self.health().returncode, 0)

    def test_receipt_for_different_recipient_is_rejected(self):
        directory = self.make_backup()
        self.assertEqual(self.health().returncode, 0)
        manifest = json.loads((directory / "manifest.json").read_text())
        self.sign(receipt_payload(manifest, "B" * 40), directory / "verified.sig")
        self.assertNotEqual(self.health().returncode, 0)

    def test_backup_from_another_gateway_is_rejected(self):
        directory = self.make_backup()
        self.assertEqual(self.health().returncode, 0)
        manifest_path = directory / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["gateway_id"] = "b" * 64
        raw = json.dumps(manifest).encode()
        manifest_path.write_bytes(raw)
        self.sign(raw, directory / "manifest.sig")
        self.sign(receipt_payload(manifest, self.recipient), directory / "verified.sig")
        self.assertNotEqual(self.health().returncode, 0)

    def test_backup_from_replaced_database_container_is_rejected(self):
        self.make_backup()
        self.assertEqual(self.health().returncode, 0)
        self.env["TEST_POSTGRES_ID"] = "c" * 64
        self.assertNotEqual(self.health().returncode, 0)

    def test_backup_from_reinitialized_database_in_same_container_is_rejected(self):
        self.make_backup()
        self.assertEqual(self.health().returncode, 0)
        self.env["TEST_DATABASE_SYSTEM_ID"] = "5678"
        self.assertNotEqual(self.health().returncode, 0)

    def test_backup_from_recreated_database_in_same_cluster_is_rejected(self):
        self.make_backup()
        self.assertEqual(self.health().returncode, 0)
        self.env["TEST_DATABASE_OID"] = "456"
        self.assertNotEqual(self.health().returncode, 0)

    def test_backup_from_replaced_storage_is_rejected(self):
        self.make_backup()
        self.assertEqual(self.health().returncode, 0)
        self.env["TEST_VOLUME_CREATED"] = "2026-11-04T00:00:00Z"
        self.assertNotEqual(self.health().returncode, 0)

    def test_invalid_docker_context_is_rejected_without_traceback(self):
        self.env["TEST_EMPTY_CONTEXT"] = "1"
        result = subprocess.run(["python3", "scripts/capture_backup_source_state.py", "--gateway", "fixture-gateway", "--postgres", "apg-postgres-1"], cwd=ROOT, env=self.env, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("Traceback", result.stderr)
        self.assertIn("無法確認備份資料來源", result.stderr)

    def test_signed_negative_database_system_identifier_is_accepted(self):
        directory = self.make_backup()
        self.env["TEST_DATABASE_SYSTEM_ID"] = "-1234"
        manifest_path = directory / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["database_system_id"] = "-1234"
        raw = json.dumps(manifest).encode()
        manifest_path.write_bytes(raw)
        self.sign(raw, directory / "manifest.sig")
        self.sign(receipt_payload(manifest, self.recipient), directory / "verified.sig")
        result = self.health()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_previous_backup_without_database_identity_is_not_reported_as_healthy(self):
        directory = self.make_backup()
        manifest_path = directory / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["version"] = 4
        del manifest["postgres_id"]
        raw = json.dumps(manifest).encode()
        manifest_path.write_bytes(raw)
        self.sign(raw, directory / "manifest.sig")
        self.sign(receipt_payload(manifest, self.recipient), directory / "verified.sig")
        self.assertNotEqual(self.health().returncode, 0)

    def test_previous_backup_without_storage_identity_is_not_reported_as_healthy(self):
        directory = self.make_backup()
        manifest_path = directory / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["version"] = 6
        del manifest["storage_identity"]
        raw = json.dumps(manifest).encode()
        manifest_path.write_bytes(raw)
        self.sign(raw, directory / "manifest.sig")
        self.sign(receipt_payload(manifest, self.recipient), directory / "verified.sig")
        self.assertNotEqual(self.health().returncode, 0)

    def test_newer_unverified_backup_is_not_hidden_by_legacy_or_previous_backup(self):
        self.make_backup()
        self.make_backup("20261103T000000Z", receipt=False)
        (self.backups / "gateway.dump").write_bytes(b"old-format")
        result = self.health()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("backup_missing", (self.root / "alerts").read_text())
        self.assertFalse((self.root / "status").exists())

    def test_stale_or_modified_manifest_is_rejected(self):
        self.make_backup(age=timedelta(hours=31))
        self.assertNotEqual(self.health().returncode, 0)
        directory = self.make_backup("20261103T000000Z")
        self.assertEqual(self.health().returncode, 0)
        manifest_path = directory / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["created_utc"] = datetime.now(timezone.utc).isoformat()
        manifest_path.write_text(json.dumps(manifest))
        self.assertNotEqual(self.health().returncode, 0)

    def test_signing_the_manifest_is_not_a_verification_receipt(self):
        directory = self.make_backup(receipt=False)
        (directory / "verified.sig").write_bytes((directory / "manifest.sig").read_bytes())
        (directory / "verified.sig").chmod(0o600)
        self.assertNotEqual(self.health().returncode, 0)

    def test_missing_file_or_future_timestamp_is_rejected(self):
        directory = self.make_backup()
        (directory / "gateway.dump.gpg").unlink()
        self.assertNotEqual(self.health().returncode, 0)
        future = self.make_backup("20261104T000000Z", age=-timedelta(hours=1))
        self.assertTrue((future / "verified.sig").is_file())
        self.assertNotEqual(self.health().returncode, 0)

    def test_freshness_does_not_check_encrypted_file_content(self):
        directory = self.make_backup()
        (directory / "gateway.dump.gpg").write_bytes(b"damaged")
        result = self.health()
        self.assertEqual(result.returncode, 0)
        self.assertIn("不重新驗證備份內容", result.stdout)

    def test_truncated_file_is_rejected(self):
        directory = self.make_backup()
        self.assertEqual(self.health().returncode, 0)
        (directory / "gateway.dump.gpg").write_bytes(b"short")
        self.assertNotEqual(self.health().returncode, 0)

    def test_size_list_requires_every_positive_integer(self):
        sizes = {name: 1 for name in FILES}
        self.assertTrue(valid_sizes(sizes))
        for value in (0, -1, "1", True, None):
            self.assertFalse(valid_sizes({**sizes, "gateway.dump.gpg": value}))
        del sizes["gateway.dump.gpg"]
        self.assertFalse(valid_sizes(sizes))

    def test_old_signed_backup_without_size_requires_new_backup(self):
        directory = self.make_backup()
        manifest_path = directory / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["version"] = 3
        del manifest["size"]
        raw = json.dumps(manifest).encode()
        manifest_path.write_bytes(raw)
        self.sign(raw, directory / "manifest.sig")
        self.sign(receipt_payload(manifest, self.recipient), directory / "verified.sig")
        self.assertNotEqual(self.health().returncode, 0)

    def test_cannot_use_old_backup_if_database_state_is_unknown(self):
        (self.backups / "gateway.dump").write_bytes(b"old-format")
        (self.bin / "docker").write_text("#!/bin/sh\nexit 1\n")
        self.assertNotEqual(self.health().returncode, 0)

    def test_database_state_check_times_out_and_alerts(self):
        self.make_backup()
        self.assertEqual(self.health().returncode, 0)
        (self.bin / "docker").write_text("#!/bin/sh\nsleep 30\nprintf 't\\n'\n")
        started = time.monotonic()
        result = self.health()
        elapsed = time.monotonic() - started
        self.assertNotEqual(result.returncode, 0)
        self.assertLess(elapsed, 19)
        self.assertIn("backup_missing", (self.root / "alerts").read_text())

    def test_legacy_database_uses_legacy_backup(self):
        self.env["TEST_SCHEMA"] = "f"
        (self.backups / "gateway.dump").write_bytes(b"legacy")
        self.assertNotEqual(self.health().returncode, 0)

    def test_unknown_gateway_source_cannot_be_reported_as_healthy_legacy_backup(self):
        (self.bin / "docker").write_text("#!/bin/sh\nprintf 'f\\n'\n")
        (self.backups / "gateway.dump").write_bytes(b"legacy")
        self.env["CAPTURE_BACKUP_GATEWAY"] = "actual-gateway"
        self.assertNotEqual(self.health().returncode, 0)


if __name__ == "__main__":
    unittest.main()
