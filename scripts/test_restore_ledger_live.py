from datetime import date, timedelta
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from ledger_backup_bundle import write_bundle


ROOT = Path(__file__).resolve().parent.parent


@unittest.skipUnless(os.environ.get("RUN_LEDGER_RESTORE_DOCKER") == "1", "requires isolated Docker PostgreSQL")
class LedgerRestoreLiveTest(unittest.TestCase):
    def test_restore_and_reject_capture_database(self):
        run_id = uuid.uuid4().hex
        label = "gateway.ledger_restore_test=" + run_id
        expiry = "expire=" + (date.today() + timedelta(days=1)).isoformat()
        databases = ["ledger-restore-" + part + "-" + run_id for part in ("source", "target", "capture")]
        volumes = ["ledger-restore-" + part + "-" + run_id for part in ("target", "capture")]

        def docker(*args, input=None):
            return subprocess.run(["docker", *args], input=input, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=90, check=True).stdout

        def sql(database, filename):
            return docker("exec", "-i", database, "psql", "-X", "-U", "gateway", "-d", "gateway", "-Atq", "-v", "ON_ERROR_STOP=1", input=(ROOT / "queries" / filename).read_bytes()).strip()

        try:
            for volume in volumes:
                docker("volume", "create", "--label", expiry, "--label", label, volume)
            for database in databases:
                docker("run", "-d", "--rm", "--label", expiry, "--label", label, "--pull=never", "--name", database, "--network", "none", "-e", "POSTGRES_USER=gateway", "-e", "POSTGRES_PASSWORD=fixture-only", "-e", "POSTGRES_DB=gateway", "postgres:17-alpine", "-c", "max_prepared_transactions=10")
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    if subprocess.run(["docker", "exec", database, "pg_isready", "-h", "127.0.0.1", "-U", "gateway"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                        break
                    time.sleep(0.2)
                else:
                    self.fail("isolated database did not start")
            sql(databases[0], "test_legacy_backup_seed.sql")
            spool = io.BytesIO()
            with tarfile.open(fileobj=spool, mode="w") as archive:
                item = tarfile.TarInfo("a" * 32 + ".ready")
                item.size = len(b"fixture pending")
                archive.addfile(item, io.BytesIO(b"fixture pending"))
            with tempfile.TemporaryDirectory(prefix="ledger-restore-live-") as temporary:
                root = Path(temporary)
                gpg_home = root / "gpg"
                gpg_home.mkdir(mode=0o700)
                env = dict(os.environ, GNUPGHOME=str(gpg_home))
                def gpg(*args, input=None):
                    return subprocess.run(["gpg", "--batch", "--yes", *args], input=input, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=60, check=True, env=env).stdout
                gpg("--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Fixture <fixture@example.invalid>", "rsa2048", "sign,encr", "0")
                listing = gpg("--with-colons", "--list-secret-keys").decode()
                fingerprint = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
                def backup(name):
                    directory = root / name
                    database = docker("exec", databases[0], "pg_dump", "-U", "gateway", "-d", "gateway", "-Fc")
                    data = {"gateway.dump.gpg": database, "ledger-spool.tar.gpg": spool.getvalue()}
                    sources = {}
                    for filename, value in data.items():
                        path = root / (name + "-" + filename + ".input")
                        path.write_bytes(value)
                        path.chmod(0o600)
                        sources[filename] = ["cat", str(path)]
                    identity = {"gateway_id": "c" * 64, "postgres_id": "a" * 64, "database_system_id": "1", "database_oid": "1", "storage_identity": "b" * 64}
                    commit = "import os,sys; from pathlib import Path; p=Path(sys.argv[1])/'.complete'; command=sys.stdin.buffer.readline();\nif command != b'commit\\n': raise SystemExit(1)\nfd=os.open(p,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600); os.write(fd,b'ok\\n'); os.fsync(fd); os.close(fd)"
                    lock = subprocess.Popen([sys.executable, "-c", commit, str(directory)], stdin=subprocess.PIPE)
                    original = os.environ.get("GNUPGHOME")
                    os.environ["GNUPGHOME"] = str(gpg_home)
                    try:
                        write_bundle(directory, sources, fingerprint, fingerprint, identity, lock)
                    finally:
                        if original is None:
                            del os.environ["GNUPGHOME"]
                        else:
                            os.environ["GNUPGHOME"] = original
                        if lock.poll() is None:
                            lock.terminate()
                        lock.wait(timeout=10)
                        if lock.stdin and not lock.stdin.closed:
                            lock.stdin.close()
                    return directory
                def restore(directory, database, volume):
                    return subprocess.run([sys.executable, "scripts/restore_ledger_backup.py", "--backup", str(directory), "--signer", fingerprint, "--recipient", fingerprint, "--postgres", database, "--ledger", volume], cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=180)
                clean = backup("clean")
                sql(databases[1], "test_ledger_prepare_transaction.sql")
                try:
                    pending = restore(clean, databases[1], volumes[0])
                    self.assertNotEqual(pending.returncode, 0)
                finally:
                    sql(databases[1], "test_ledger_rollback_prepared.sql")
                self.assertEqual(sql(databases[1], "capture_restore_preflight.sql"), b"0")
                encrypted_path = clean / "gateway.dump.gpg"
                encrypted = encrypted_path.read_bytes()
                altered = bytearray(encrypted)
                altered[len(altered) // 2] ^= 1
                encrypted_path.write_bytes(altered)
                corrupt = restore(clean, databases[1], volumes[0])
                self.assertNotEqual(corrupt.returncode, 0)
                self.assertEqual(sql(databases[1], "capture_restore_preflight.sql"), b"0")
                encrypted_path.write_bytes(encrypted)
                result = restore(clean, databases[1], volumes[0])
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                self.assertEqual(sql(databases[1], "test_legacy_backup_count.sql"), b"1")
                payload = docker("run", "--rm", "--label", expiry, "--label", label, "--pull=never", "--network", "none", "--mount", f"type=volume,source={volumes[0]},target=/spool,readonly", "--entrypoint", "cat", "python:3.13-slim", "/spool/" + "a" * 32 + ".ready")
                self.assertEqual(payload, b"fixture pending")
                sql(databases[0], "test_ledger_restore_capture_seed.sql")
                forbidden = backup("contains-capture")
                rejected = restore(forbidden, databases[2], volumes[1])
                self.assertNotEqual(rejected.returncode, 0)
                self.assertEqual(sql(databases[2], "capture_backup_schema_present.sql"), b"t")
                empty = docker("run", "--rm", "--label", expiry, "--label", label, "--pull=never", "--network", "none", "--mount", f"type=volume,source={volumes[1]},target=/spool,readonly", "--entrypoint", "sh", "python:3.13-slim", "-c", "test -z \"$(ls -A /spool)\"")
                self.assertEqual(empty, b"")
        finally:
            for database in databases:
                subprocess.run(["docker", "stop", "--time", "2", database], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
            for volume in volumes:
                subprocess.run(["docker", "volume", "rm", volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
            for command in (["docker", "ps", "-aq"], ["docker", "volume", "ls", "-q"]):
                self.assertEqual(subprocess.run([*command, "--filter", "label=" + label], capture_output=True, timeout=10, check=True).stdout.strip(), b"")


if __name__ == "__main__":
    unittest.main()
