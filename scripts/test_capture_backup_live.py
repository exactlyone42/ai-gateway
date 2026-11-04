import json
from datetime import datetime, timedelta, timezone
import hashlib
import io
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tarfile
import tempfile
import time
import unittest
import uuid
try:
    from scripts.capture_postgres import local_exec
except ModuleNotFoundError:
    from capture_postgres import local_exec


SCRIPT = Path(__file__).with_name("capture_backup.py")
VERIFY = Path(__file__).with_name("verify_capture_backup.py")
RESTORE = Path(__file__).with_name("restore_capture_backup.py")
EXPIRE = f"expire={(datetime.now(timezone.utc) + timedelta(days=1)).date().isoformat()}"


@unittest.skipUnless(os.environ.get("RUN_CAPTURE_BACKUP_DOCKER") == "1", "需要隔離 Docker 驗收")
class CaptureBackupLiveTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="capture-backup-live-")
        cls.base = Path(cls.temp.name)
        cls.env = os.environ.copy()
        home = cls.base / "gnupg"
        home.mkdir(mode=0o700)
        cls.env["GNUPGHOME"] = str(home)
        subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Capture Docker Test <capture-docker@example.invalid>", "rsa2048", "encr", "0"], env=cls.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        listing = subprocess.run(["gpg", "--batch", "--with-colons", "--list-keys"], env=cls.env, capture_output=True, text=True, check=True).stdout
        cls.recipient = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
        subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Capture Docker Signer <capture-sign@example.invalid>", "rsa2048", "sign", "0"], env=cls.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        listing = subprocess.run(["gpg", "--batch", "--with-colons", "--list-secret-keys", "capture-sign@example.invalid"], env=cls.env, capture_output=True, text=True, check=True).stdout
        cls.signer = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
        suffix = uuid.uuid4().hex[:12]
        cls.gateway = f"capture-backup-gateway-{suffix}"
        cls.postgres = f"capture-backup-postgres-{suffix}"
        cls.network = f"capture-backup-network-{suffix}"
        cls.volumes = [f"capture-backup-{suffix}-{name}" for name in ("spool", "keys", "ledger", "erasure", "witness")]
        cls.resources = []
        real_docker = shutil.which("docker")
        if not real_docker:
            raise RuntimeError("找不到 Docker")
        wrapper = cls.base / "docker"
        wrapper.write_text(
            "#!/usr/bin/python3\nimport json,os,sys\n"
            + "args=sys.argv[1:]\n"
            + "if len(args) >= 4 and args[0] == 'exec' and args[2:] == ['/proc/1/exe', '--inspect-capture-erasure']:\n"
            + "    print(json.dumps({'install':'00000000-0000-4000-8000-000000000001','sequence':0,'tip':[0]*32})); sys.exit(0)\n"
            + "os.execv(" + repr(real_docker) + ", [" + repr(real_docker) + "] + args)\n"
        )
        wrapper.chmod(0o755)
        cls.env["PATH"] = str(cls.base) + os.pathsep + cls.env["PATH"]
        try:
            subprocess.run(["docker", "network", "create", "--label", EXPIRE, cls.network], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            cls.resources.append(cls.network)
            for volume in cls.volumes:
                subprocess.run(["docker", "volume", "create", "--label", EXPIRE, volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
                cls.resources.append(volume)
            paths = ["/var/lib/agent-pool-gateway/capture-spool", "/var/lib/agent-pool-gateway/capture-keys", "/var/lib/agent-pool-gateway/ledger-spool", "/var/lib/agent-pool-gateway/capture-erasure", "/var/lib/agent-pool-gateway/capture-witness"]
            mounts = []
            for volume, path in zip(cls.volumes, paths):
                mounts.extend(["--mount", f"type=volume,source={volume},target={path}"])
            init = "umask 077; touch /var/lib/agent-pool-gateway/capture-spool/.capture-backup.lock /var/lib/agent-pool-gateway/ledger-spool/.capture-backup.lock; printf synthetic-capture > /var/lib/agent-pool-gateway/capture-spool/00000000-0000-0000-0000-000000000001.capture; printf synthetic-ledger > /var/lib/agent-pool-gateway/ledger-spool/test; printf synthetic-conflict > /var/lib/agent-pool-gateway/ledger-spool/00000000000000000000000000000001.conflict; exec sleep 300"
            env_keys = ["CONTENT_CAPTURE_SPOOL_DIR", "CONTENT_CAPTURE_KEY_DIR", "LEDGER_SPOOL_DIR", "CONTENT_CAPTURE_ERASURE_DIR", "CONTENT_CAPTURE_WITNESS_DIR"]
            env_args = sum((["-e", key + "=" + path] for key, path in zip(env_keys, paths)), [])
            subprocess.run(["docker", "run", "-d", "--rm", "--pull=never", "--name", cls.gateway, "--network", cls.network, "-e", f"DATABASE_URL=postgres://gateway:synthetic-test-only@{cls.postgres}:5432/gateway", *env_args, *mounts, "--entrypoint", "sh", "postgres:17-alpine", "-c", init], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            cls.resources.append(cls.gateway)
            cls.old_key = b"synthetic-private-key-32-bytes!!"
            assert len(cls.old_key) == 32
            cls.old_fingerprint = hashlib.sha256(cls.old_key).digest()
            keyfile = f"/var/lib/agent-pool-gateway/capture-keys/{cls.old_fingerprint.hex()}.key"
            subprocess.run(["docker", "exec", "-i", cls.gateway, "sh", "-c", f"umask 077; dd bs=32 count=1 of={keyfile} 2>/dev/null"], input=cls.old_key, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            subprocess.run(["docker", "exec", "-i", cls.gateway, "sh", "-c", "umask 077; dd bs=32 count=1 of=/var/lib/agent-pool-gateway/capture-keys/current 2>/dev/null"], input=cls.old_fingerprint, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            fixture = os.environ.get("CAPTURE_FIXTURE_ROOT")
            if fixture:
                source = Path(fixture) / "spool" / "00000000-0000-0000-0000-000000000001.capture"
                subprocess.run(["docker", "exec", "-i", cls.gateway, "sh", "-c", "dd of=/var/lib/agent-pool-gateway/capture-spool/00000000-0000-0000-0000-000000000001.capture 2>/dev/null"], input=source.read_bytes(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            subprocess.run(["docker", "run", "-d", "--rm", "--pull=never", "--name", cls.postgres, "--network", cls.network, "-e", "POSTGRES_USER=gateway", "-e", "POSTGRES_PASSWORD=synthetic-test-only", "-e", "POSTGRES_DB=gateway", "postgres:17-alpine"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            cls.resources.append(cls.postgres)
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                ready = subprocess.run(["docker", "exec", cls.postgres, "psql", "-h", "127.0.0.1", "-U", "gateway", "-d", "gateway", "-Atqc", "SELECT 1"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                if ready.returncode == 0:
                    break
                time.sleep(0.2)
            else:
                raise RuntimeError("隔離資料庫沒有啟動")
            subprocess.run(["docker", "exec", cls.postgres, "psql", "-U", "gateway", "-d", "gateway", "-c", "CREATE TABLE capture_master_key_usage (key_fingerprint bytea PRIMARY KEY, wrap_count bigint NOT NULL); CREATE TABLE capture_restore_fixture (value bytea NOT NULL); INSERT INTO capture_restore_fixture VALUES (decode('01020304','hex'));"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            migration = Path(__file__).resolve().parent.parent / "migrations/20261102000001_capture_erasure_floor.sql"
            subprocess.run(["docker", "exec", "-i", cls.postgres, "psql", "-U", "gateway", "-d", "gateway", "-v", "ON_ERROR_STOP=1"], input=migration.read_bytes(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        except Exception:
            cls.cleanup_resources()
            subprocess.run(["gpgconf", "--kill", "gpg-agent"], env=cls.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
            cls.temp.cleanup()
            raise

    @classmethod
    def cleanup_resources(cls):
        for name in (cls.gateway, cls.postgres):
            if name in cls.resources:
                subprocess.run(["docker", "rm", "-f", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        for volume in cls.volumes:
            if volume in cls.resources:
                subprocess.run(["docker", "volume", "rm", volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        if cls.network in cls.resources:
            subprocess.run(["docker", "network", "rm", cls.network], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)

    @classmethod
    def tearDownClass(cls):
        cls.cleanup_resources()
        subprocess.run(["gpgconf", "--kill", "gpg-agent"], env=cls.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        cls.temp.cleanup()

    def test_expanded_database_dump_limit(self):
        root = Path(__file__).resolve().parent.parent
        containers = ["docker", "ps", "-aq", "--filter", "label=agent-pool-gateway.capture-preflight=1"]
        before = set(subprocess.check_output(containers, text=True).splitlines())
        psql = ["docker", "exec", "-i", self.postgres, "psql", "-U", "gateway", "-d", "gateway", "-v", "ON_ERROR_STOP=1"]
        subprocess.run(psql, input=(root / "queries/test_capture_expansion_fixture.sql").read_bytes(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        try:
            dump = subprocess.run(["docker", "exec", self.postgres, "pg_dump", "-U", "gateway", "-d", "gateway", "-Fc", "-t", "capture_expansion_fixture"], capture_output=True, check=True).stdout
            encrypted = self.base / "expansion-dump.gpg"
            subprocess.run(["gpg", "--batch", "--yes", "--trust-model", "always", "--recipient", self.recipient, "--encrypt", "--output", str(encrypted)], input=dump, env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            encrypted.chmod(0o600)
            self.assertLess(encrypted.stat().st_size, 200000)
            probe = "import sys; from restore_capture_backup import check_dump_expansion; from pathlib import Path; check_dump_expansion(Path(sys.argv[1]), sys.argv[2], int(sys.argv[3]))"
            command = ["python3", "-c", probe, str(encrypted), self.postgres]
            low = subprocess.run(command + ["200000"], cwd=root / "scripts", env=self.env, capture_output=True, text=True, timeout=60)
            self.assertNotEqual(low.returncode, 0)
            self.assertIn("資料庫備份展開超過還原上限", low.stderr)
            normal = subprocess.run(command + ["30000000"], cwd=root / "scripts", env=self.env, capture_output=True, text=True, timeout=60)
            self.assertEqual(normal.returncode, 0, normal.stderr)
            damaged = self.base / "expansion-damaged.gpg"
            damaged.write_bytes(encrypted.read_bytes()[:-32])
            damaged.chmod(0o600)
            damaged_command = command.copy()
            damaged_command[-2] = str(damaged)
            broken = subprocess.run(damaged_command + ["30000000"], cwd=root / "scripts", env=self.env, capture_output=True, text=True, timeout=60)
            self.assertNotEqual(broken.returncode, 0)
            after = set(subprocess.check_output(containers, text=True).splitlines())
            self.assertEqual(after - before, set())
        finally:
            subprocess.run(psql, input=(root / "queries/test_capture_expansion_cleanup.sql").read_bytes(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)

    def test_real_container_mounts_database_and_gpg(self):
        destination = self.base / "snapshot"
        result = subprocess.run(["python3", str(SCRIPT), "--gateway", self.gateway, "--postgres", self.postgres, "--destination", str(destination), "--recipient", self.recipient, "--signer", self.signer], env=self.env, capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((destination / ".complete").read_bytes(), b"ok\n")
        manifest = json.loads((destination / "manifest.json").read_text())
        self.assertEqual(len(manifest["sha256"]), 4)
        for name in manifest["sha256"]:
            self.assertNotIn(b"synthetic-private-key", (destination / name).read_bytes())
        key_tar = self.base / "key.tar"
        with key_tar.open("wb") as output:
            subprocess.run(["gpg", "--batch", "--decrypt", str(destination / "capture-keys.tar.gpg")], env=self.env, stdout=output, stderr=subprocess.DEVNULL, check=True)
        with tarfile.open(key_tar) as archive:
            self.assertEqual(archive.extractfile(f"./{self.old_fingerprint.hex()}.key").read(), self.old_key)
        dump = subprocess.run(["gpg", "--batch", "--decrypt", str(destination / "gateway.dump.gpg")], env=self.env, capture_output=True, check=True).stdout
        self.assertTrue(dump.startswith(b"PGDMP"))
        listing = subprocess.run(["docker", "exec", "-i", self.postgres, "pg_restore", "--list"], input=dump, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.assertEqual(listing.returncode, 0)
        verification = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True, timeout=60)
        self.assertEqual(verification.returncode, 0, verification.stderr)
        with (destination / "capture-spool.tar.gpg").open("ab") as output:
            output.write(b"modified")
        corrupted = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True, timeout=60)
        self.assertNotEqual(corrupted.returncode, 0)

    def test_z_database_then_ledger_snapshot_can_miss_moved_entry(self):
        ledger_path = "/var/lib/agent-pool-gateway/ledger-spool/race-fixture"
        lock_path = "/var/lib/agent-pool-gateway/capture-spool/.capture-backup.lock"
        lock = subprocess.Popen(["docker", "exec", "-i", self.gateway, "flock", "-x", lock_path, "sh", "-c", "echo HELD; read release"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        try:
            self.assertEqual(lock.stdout.readline().strip(), b"HELD")
            subprocess.run(["docker", "exec", self.gateway, "sh", "-c", f"printf pending > {ledger_path}"], check=True)
            dump = subprocess.run(["docker", "exec", self.postgres, "pg_dump", "-U", "gateway", "-d", "gateway", "-Fc"], capture_output=True, check=True).stdout
            sql = (Path(__file__).parent.parent / "queries/test_backup_race_insert.sql").read_bytes()
            subprocess.run(["docker", "exec", "-i", self.postgres, "psql", "-v", "ON_ERROR_STOP=1", "-U", "gateway", "-d", "gateway"], input=sql, stdout=subprocess.DEVNULL, check=True)
            subprocess.run(["docker", "exec", self.gateway, "rm", ledger_path], check=True)
            dumped = subprocess.run(["docker", "exec", "-i", self.postgres, "pg_restore", "--data-only", "--table=capture_restore_fixture", "-f", "-"], input=dump, capture_output=True, check=True).stdout
            ledger = subprocess.run(["docker", "exec", self.gateway, "tar", "-C", "/var/lib/agent-pool-gateway/ledger-spool", "-cf", "-", "."], capture_output=True, check=True).stdout
            self.assertIn(b"01020304", dumped)
            self.assertNotIn(b"11121314", dumped)
            with tarfile.open(fileobj=io.BytesIO(ledger)) as archive:
                self.assertIn("./test", archive.getnames())
                self.assertNotIn("./race-fixture", archive.getnames())
        finally:
            lock.stdin.close()
            lock.wait(timeout=10)
            lock.stdout.close()

    def test_real_active_writer_lock_prevents_backup(self):
        for directory in ("capture-spool", "ledger-spool"):
            lock_path = f"/var/lib/agent-pool-gateway/{directory}/.capture-backup.lock"
            worker = subprocess.Popen(["docker", "exec", "-i", self.gateway, "flock", "-s", lock_path, "sh", "-c", "echo HELD; read release"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            try:
                self.assertEqual(worker.stdout.readline().strip(), b"HELD")
                destination = self.base / f"blocked-{directory}"
                result = subprocess.run(["python3", str(SCRIPT), "--gateway", self.gateway, "--postgres", self.postgres, "--destination", str(destination), "--recipient", self.recipient, "--signer", self.signer], env=self.env, capture_output=True, text=True, timeout=30)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((destination / ".complete").exists())
            finally:
                if worker.stdin:
                    worker.stdin.close()
                worker.wait(timeout=10)
                if worker.stdout:
                    worker.stdout.close()

    def test_postgres_environment_cannot_redirect_local_connection(self):
        root = Path(__file__).resolve().parent.parent
        injected = sum((["-e", name + "=" + value] for name, value in [("PGHOST", "127.0.0.2"), ("PGHOSTADDR", "127.0.0.2"), ("PGPORT", "1"), ("PGSERVICE", "not-a-service"), ("PGDATABASE", "wrong"), ("PGUSER", "wrong")]), [])
        query = (root / "queries/test_capture_backup_database_identity.sql").read_bytes()
        naive = subprocess.run(["docker", "exec", "-i", *injected, self.postgres, "psql", "-U", "gateway", "-d", "gateway", "-Atq"], input=query, capture_output=True, timeout=15)
        self.assertNotEqual(naive.returncode, 0)
        command = local_exec(self.postgres, 5432, "psql")
        command[3:3] = injected
        safe = subprocess.run(command + ["-U", "gateway", "-d", "gateway", "-Atq", "-v", "ON_ERROR_STOP=1"], input=query, capture_output=True, timeout=15)
        self.assertEqual(safe.returncode, 0, safe.stderr.decode())
        self.assertEqual(safe.stdout.decode().strip(), "t")

    def test_incomplete_key_initialization_prevents_backup(self):
        for name in (".initializing", ".initializing-incomplete"):
            marker = f"/var/lib/agent-pool-gateway/capture-keys/{name}"
            subprocess.run(["docker", "exec", self.gateway, "touch", marker], check=True)
            try:
                destination = self.base / f"blocked-{name}"
                result = subprocess.run(["python3", str(SCRIPT), "--gateway", self.gateway, "--postgres", self.postgres, "--destination", str(destination), "--recipient", self.recipient, "--signer", self.signer], env=self.env, capture_output=True, text=True, timeout=30)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((destination / ".complete").exists())
            finally:
                subprocess.run(["docker", "exec", self.gateway, "rm", marker], check=True)

    def test_floor_update_and_restore_marker_commit_together(self):
        target = f"capture-floor-atomic-{uuid.uuid4().hex[:12]}"
        root = Path(__file__).resolve().parent.parent
        subprocess.run(["docker", "run", "-d", "--rm", "--label", EXPIRE, "--pull=never", "--network", "none", "--name", target, "-e", "POSTGRES_USER=gateway", "-e", "POSTGRES_PASSWORD=synthetic-test-only", "-e", "POSTGRES_DB=gateway", "postgres:17-alpine"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        try:
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                ready = subprocess.run(["docker", "exec", target, "pg_isready", "-h", "127.0.0.1", "-U", "gateway"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                if ready.returncode == 0:
                    break
                time.sleep(0.2)
            else:
                self.fail("隔離資料庫沒有啟動")
            psql = ["docker", "exec", "-i", target, "psql", "-U", "gateway", "-d", "gateway", "-Atq", "-v", "ON_ERROR_STOP=1"]
            def execute(file, install="00000000-0000-4000-8000-000000000001", sequence=2, tip="05" * 32):
                return subprocess.run(psql + ["-v", "install=" + install, "-v", "sequence=" + str(sequence), "-v", "tip=" + tip], input=(root / file).read_bytes(), capture_output=True)
            self.assertEqual(execute("migrations/20261102000001_capture_erasure_floor.sql").returncode, 0)
            self.assertEqual(execute("queries/capture_restore_begin.sql").returncode, 0)
            self.assertEqual(execute("queries/test_capture_restore_floor_seed.sql").returncode, 0)
            for install, sequence, tip in [
                ("00000000-0000-4000-8000-000000000001", 1, "05" * 32),
                ("00000000-0000-4000-8000-000000000001", 2, "06" * 32),
                ("00000000-0000-4000-8000-000000000002", 3, "05" * 32),
            ]:
                self.assertNotEqual(execute("queries/capture_restore_finish.sql", install, sequence, tip).returncode, 0)
                state = execute("queries/test_capture_restore_floor_state.sql")
                self.assertEqual(state.returncode, 0)
                self.assertEqual(state.stdout.decode().strip(), "t|00000000-0000-4000-8000-000000000001|2|" + "05" * 32)
            self.assertEqual(execute("queries/capture_restore_finish.sql", sequence=3, tip="06" * 32).returncode, 0)
            state = execute("queries/test_capture_restore_floor_state.sql")
            self.assertEqual(state.stdout.decode().strip(), "f|00000000-0000-4000-8000-000000000001|3|" + "06" * 32)
        finally:
            subprocess.run(["docker", "rm", "-f", "-v", target], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)

    def test_killed_restore_keeps_marker_and_gateway_refuses_startup(self):
        backup = self.base / "interrupted-source"
        source = subprocess.run(["python3", str(SCRIPT), "--gateway", self.gateway, "--postgres", self.postgres, "--destination", str(backup), "--recipient", self.recipient, "--signer", self.signer], env=self.env, capture_output=True, text=True, timeout=60)
        self.assertEqual(source.returncode, 0, source.stderr)
        suffix = uuid.uuid4().hex[:12]
        target = f"capture-restore-interrupted-{suffix}"
        volumes = [f"capture-interrupted-{suffix}-{part}" for part in ("spool", "keys", "ledger", "anchors")]
        created = []
        started = False
        root = Path(__file__).resolve().parent.parent
        try:
            for volume in volumes:
                subprocess.run(["docker", "volume", "create", "--label", EXPIRE, volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
                created.append(volume)
            subprocess.run(["docker", "run", "-d", "--rm", "--label", EXPIRE, "--pull=never", "--network", "none", "--name", target, "-e", "POSTGRES_USER=gateway", "-e", "POSTGRES_PASSWORD=synthetic-test-only", "-e", "POSTGRES_DB=gateway", "postgres:17-alpine"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            started = True
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                ready = subprocess.run(["docker", "exec", target, "pg_isready", "-h", "127.0.0.1", "-U", "gateway"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                if ready.returncode == 0:
                    break
                time.sleep(0.2)
            else:
                self.fail("隔離還原資料庫沒有啟動")
            probe = '''import argparse,os,signal,sys
sys.path.insert(0, sys.argv[1])
import restore_capture_backup as restore
def interrupt(*args):
    os.kill(os.getpid(), signal.SIGKILL)
restore.rotate_restored_key = interrupt
args = argparse.Namespace(backup=__import__("pathlib").Path(sys.argv[2]), signer=sys.argv[3], postgres=sys.argv[4], spool=sys.argv[5], keys=sys.argv[6], ledger=sys.argv[7], anchors=sys.argv[8], db_user="gateway", db_name="gateway", uid=10001, gid=999, max_bytes=16777216, staging=__import__("pathlib").Path(sys.argv[9]))
restore.restore(args)
'''
            interrupted = subprocess.run(["python3", "-c", probe, str(root / "scripts"), str(backup), self.signer, target, *volumes, str(self.base)], env=self.env, capture_output=True, text=True, timeout=120)
            self.assertEqual(interrupted.returncode, -signal.SIGKILL, interrupted.stderr)
            psql = ["docker", "exec", "-i", target, "psql", "-U", "gateway", "-d", "gateway", "-Atq", "-v", "ON_ERROR_STOP=1"]
            restored = subprocess.run(psql, input=(root / "queries/test_capture_restore_fixture_read.sql").read_bytes(), capture_output=True, check=True)
            self.assertEqual(restored.stdout.decode().strip(), "01020304")
            marker = subprocess.run(psql, input=(root / "queries/capture_restore_incomplete.sql").read_bytes(), capture_output=True, check=True)
            self.assertEqual(marker.stdout.decode().strip(), "t")
            binary = (root / "target/debug/agent-pool-gateway").resolve(strict=True)
            command = ["docker", "run", "--rm", "--label", EXPIRE, "--pull=never", "--network", "container:" + target, "--mount", f"type=bind,source={binary},target=/tmp/gateway,readonly", "-e", "DATABASE_URL=postgres://gateway:synthetic-test-only@127.0.0.1:5432/gateway", "-e", "DATABASE_MAX_CONNECTIONS=5", "-e", "ADMIN_DATABASE_MAX_CONNECTIONS=1", "-e", "CREDENTIAL_HASH_KEY=fixture-hash-key-with-at-least-32-bytes", "-e", "CREDENTIAL_ENVIRONMENT=dev", "-e", "PROVIDER_ALLOWED_TENANT_IDS=00000000-0000-0000-0000-000000000001", "-e", "CONTENT_CAPTURE_ENABLED=false", "--entrypoint", "/tmp/gateway", "python:3.13-slim"]
            gateway = subprocess.run(command, capture_output=True, text=True, timeout=30)
            self.assertNotEqual(gateway.returncode, 0)
            self.assertIn("capture restore incomplete", gateway.stderr)
        finally:
            if started:
                subprocess.run(["docker", "rm", "-f", "-v", target], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            for volume in created:
                subprocess.run(["docker", "volume", "rm", volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)

    def test_restore_only_to_empty_isolated_targets_and_rotate_key(self):
        backup = self.base / "restore-source"
        created = subprocess.run(["python3", str(SCRIPT), "--gateway", self.gateway, "--postgres", self.postgres, "--destination", str(backup), "--recipient", self.recipient, "--signer", self.signer], env=self.env, capture_output=True, text=True, timeout=60)
        self.assertEqual(created.returncode, 0, created.stderr)
        suffix = uuid.uuid4().hex[:12]
        target = f"capture-restore-db-{suffix}"
        volumes = [f"capture-restore-{suffix}-{name}" for name in ("spool", "keys", "ledger", "anchors")]
        created_volumes = []
        started = False
        try:
            for volume in volumes:
                subprocess.run(["docker", "volume", "create", "--label", EXPIRE, volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
                created_volumes.append(volume)
            subprocess.run(["docker", "run", "-d", "--rm", "--pull=never", "--network", "none", "--name", target, "-e", "POSTGRES_USER=gateway", "-e", "POSTGRES_PASSWORD=synthetic-test-only", "-e", "POSTGRES_DB=gateway", "postgres:17-alpine"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            started = True
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                ready = subprocess.run(["docker", "exec", target, "psql", "-h", "127.0.0.1", "-U", "gateway", "-d", "gateway", "-Atqc", "SELECT 1"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                if ready.returncode == 0:
                    break
                time.sleep(0.2)
            else:
                self.fail("隔離還原資料庫沒有啟動")
            command = ["python3", str(RESTORE), "--backup", str(backup), "--signer", self.signer,
                       "--postgres", target, "--spool", volumes[0], "--keys", volumes[1],
                       "--ledger", volumes[2], "--anchors", volumes[3], "--staging", str(self.base)]
            root = Path(__file__).resolve().parent.parent
            source_psql = ["docker", "exec", "-i", self.postgres, "psql", "-U", "gateway", "-d", "gateway", "-v", "ON_ERROR_STOP=1"]
            subprocess.run(source_psql, input=(root / "queries/test_capture_expansion_fixture.sql").read_bytes(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            try:
                expanded = self.base / "expanded-restore-source"
                expansion_backup = subprocess.run(["python3", str(SCRIPT), "--gateway", self.gateway, "--postgres", self.postgres, "--destination", str(expanded), "--recipient", self.recipient, "--signer", self.signer], env=self.env, capture_output=True, text=True, timeout=60)
                self.assertEqual(expansion_backup.returncode, 0, expansion_backup.stderr)
                self.assertLess(sum(item.stat().st_size for item in expanded.iterdir() if item.is_file()), 200000)
            finally:
                subprocess.run(source_psql, input=(root / "queries/test_capture_expansion_cleanup.sql").read_bytes(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            expanded_command = command.copy()
            expanded_command[expanded_command.index("--backup") + 1] = str(expanded)
            rejected_expansion = subprocess.run(expanded_command + ["--max-bytes", "200000"], env=self.env, capture_output=True, text=True, timeout=60)
            self.assertNotEqual(rejected_expansion.returncode, 0)
            probe = "import sys; from pathlib import Path; from types import SimpleNamespace; from restore_capture_backup import restore; restore(SimpleNamespace(backup=Path(sys.argv[1]), signer=sys.argv[2], postgres=sys.argv[3], spool=sys.argv[4], keys=sys.argv[5], ledger=sys.argv[6], anchors=sys.argv[7], staging=Path(sys.argv[8]), db_user='gateway', db_name='gateway', uid=10001, gid=999, max_bytes=200000))"
            diagnosed = subprocess.run(["python3", "-c", probe, str(expanded), self.signer, target, *volumes, str(self.base)], cwd=root / "scripts", env=self.env, capture_output=True, text=True, timeout=60)
            self.assertNotEqual(diagnosed.returncode, 0)
            self.assertIn("資料庫備份展開超過還原上限", diagnosed.stderr)
            marker = subprocess.run(["docker", "exec", "-i", target, "psql", "-U", "gateway", "-d", "gateway", "-Atq"], input=(root / "queries/capture_restore_incomplete.sql").read_bytes(), capture_output=True, check=True)
            self.assertEqual(marker.stdout.decode().strip(), "f")
            for volume in volumes:
                subprocess.run(["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target=/target", "--entrypoint", "sh", "postgres:17-alpine", "-c", 'test -z "$(ls -A /target)"'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            subprocess.run(["docker", "exec", target, "psql", "-U", "gateway", "-d", "gateway", "-c", "CREATE TABLE restore_guard_fixture (value integer); INSERT INTO restore_guard_fixture VALUES (17)"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            occupied = subprocess.run(command, env=self.env, capture_output=True, text=True, timeout=60)
            self.assertNotEqual(occupied.returncode, 0)
            preserved = subprocess.run(["docker", "exec", target, "psql", "-U", "gateway", "-d", "gateway", "-Atqc", "SELECT value FROM restore_guard_fixture"], capture_output=True, text=True, check=True).stdout.strip()
            self.assertEqual(preserved, "17")
            for volume in volumes:
                subprocess.run(["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target=/target", "--entrypoint", "sh", "postgres:17-alpine", "-c", 'test -z "$(ls -A /target)"'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            subprocess.run(["docker", "exec", target, "psql", "-U", "gateway", "-d", "gateway", "-c", "DROP TABLE restore_guard_fixture"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            subprocess.run(["docker", "exec", target, "psql", "-U", "gateway", "-d", "gateway", "-c", "CREATE SCHEMA pgxexisting; CREATE TABLE pgxexisting.fixture (value integer); INSERT INTO pgxexisting.fixture VALUES (23)"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            occupied_schema = subprocess.run(command, env=self.env, capture_output=True, text=True, timeout=60)
            self.assertNotEqual(occupied_schema.returncode, 0)
            preserved_schema = subprocess.run(["docker", "exec", target, "psql", "-U", "gateway", "-d", "gateway", "-Atqc", "SELECT value FROM pgxexisting.fixture"], capture_output=True, text=True, check=True).stdout.strip()
            self.assertEqual(preserved_schema, "23")
            for volume in volumes:
                subprocess.run(["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target=/target", "--entrypoint", "sh", "postgres:17-alpine", "-c", 'test -z "$(ls -A /target)"'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            subprocess.run(["docker", "exec", target, "psql", "-U", "gateway", "-d", "gateway", "-c", "DROP SCHEMA pgxexisting CASCADE"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            legacy = self.base / "legacy-restore-source"
            shutil.copytree(backup, legacy)
            legacy_manifest = legacy / "manifest.json"
            legacy_data = json.loads(legacy_manifest.read_text())
            legacy_data["version"] = 2
            legacy_data.pop("erasure_position")
            legacy_manifest.write_text(json.dumps(legacy_data))
            subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(legacy / "manifest.sig"), str(legacy_manifest)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            (legacy / "manifest.sig").chmod(0o600)
            legacy_command = command.copy()
            legacy_command[legacy_command.index("--backup") + 1] = str(legacy)
            legacy_result = subprocess.run(legacy_command, env=self.env, capture_output=True, text=True, timeout=60)
            self.assertNotEqual(legacy_result.returncode, 0)
            root = Path(__file__).resolve().parent.parent
            marker = subprocess.run(["docker", "exec", "-i", target, "psql", "-U", "gateway", "-d", "gateway", "-Atq"], input=(root / "queries/capture_restore_incomplete.sql").read_bytes(), capture_output=True, check=True)
            self.assertEqual(marker.stdout.decode().strip(), "f")
            for volume in volumes:
                subprocess.run(["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target=/target", "--entrypoint", "sh", "postgres:17-alpine", "-c", 'test -z "$(ls -A /target)"'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            bad = self.base / "unsafe-restore-source"
            shutil.copytree(backup, bad)
            content = io.BytesIO()
            with tarfile.open(fileobj=content, mode="w") as archive:
                entry = tarfile.TarInfo("../escape")
                entry.size = 1
                archive.addfile(entry, io.BytesIO(b"x"))
            encrypted = bad / "capture-spool.tar.gpg"
            subprocess.run(["gpg", "--batch", "--yes", "--trust-model", "always", "--recipient", self.recipient, "--encrypt", "--output", str(encrypted)], input=content.getvalue(), env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            manifest_path = bad / "manifest.json"
            manifest = json.loads(manifest_path.read_text())
            manifest["sha256"][encrypted.name] = hashlib.sha256(encrypted.read_bytes()).hexdigest()
            manifest_path.write_text(json.dumps(manifest))
            subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(bad / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            checked = subprocess.run(["python3", str(VERIFY), str(bad), "--signer", self.signer], env=self.env, capture_output=True, text=True, timeout=60)
            self.assertEqual(checked.returncode, 0, checked.stderr)
            rejected = subprocess.run([*command[:3], str(bad), *command[4:]], env=self.env, capture_output=True, text=True, timeout=60)
            self.assertNotEqual(rejected.returncode, 0)
            for volume in volumes:
                subprocess.run(["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target=/target", "--entrypoint", "sh", "postgres:17-alpine", "-c", 'test -z "$(ls -A /target)"'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            restored = subprocess.run(command, env=self.env, capture_output=True, text=True, timeout=120)
            self.assertEqual(restored.returncode, 0, restored.stderr)
            value = subprocess.run(["docker", "exec", target, "psql", "-U", "gateway", "-d", "gateway", "-Atqc", "SELECT encode(value, 'hex') FROM capture_restore_fixture"], capture_output=True, text=True, check=True).stdout.strip()
            self.assertEqual(value, "01020304")
            key_mount = f"type=volume,source={volumes[1]},target=/keys,readonly"
            pointer = subprocess.run(["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", key_mount, "--entrypoint", "sh", "postgres:17-alpine", "-c", "od -An -v -tx1 /keys/current | tr -d ' \\n'"], capture_output=True, text=True, check=True).stdout
            self.assertEqual(len(pointer), 64)
            self.assertNotEqual(pointer, self.old_fingerprint.hex())
            subprocess.run(["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", key_mount, "--entrypoint", "sh", "postgres:17-alpine", "-c", f"test -f /keys/{self.old_fingerprint.hex()}.key && test -f /keys/{pointer}.key"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            anchor_mount = f"type=volume,source={volumes[3]},target=/anchors,readonly"
            anchor = subprocess.run(["docker", "run", "--rm", "--pull=never", "--network", "none", "--user", "10001:999", "--mount", anchor_mount, "--entrypoint", "cat", "postgres:17-alpine", f"/anchors/{pointer}.anchor"], capture_output=True, check=True).stdout
            self.assertEqual(len(anchor), 81)
            self.assertEqual(anchor[:8], b"APGANC01")
            self.assertEqual(anchor[8:40].hex(), pointer)
            self.assertEqual(hashlib.sha256(anchor[:49]).digest(), anchor[49:])
            owner = subprocess.run(["docker", "run", "--rm", "--pull=never", "--network", "none", "--user", "10001:999", "--mount", f"type=volume,source={volumes[3]},target=/anchors", "--entrypoint", "sh", "postgres:17-alpine", "-c", 'test -w /anchors && stat -c "%u:%g:%a" /anchors'], capture_output=True, text=True, check=True).stdout.strip()
            self.assertEqual(owner, "10001:999:700")
            fixture = os.environ.get("CAPTURE_FIXTURE_ROOT")
            expected_capture = (Path(fixture) / "spool" / "00000000-0000-0000-0000-000000000001.capture").read_bytes() if fixture else b"synthetic-capture"
            for volume, path, expected in ((volumes[0], "/target/00000000-0000-0000-0000-000000000001.capture", expected_capture), (volumes[2], "/target/test", b"synthetic-ledger"), (volumes[2], "/target/00000000000000000000000000000001.conflict", b"synthetic-conflict")):
                contents = subprocess.run(["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target=/target,readonly", "--entrypoint", "cat", "postgres:17-alpine", path], capture_output=True, check=True).stdout
                self.assertEqual(contents, expected)
                if fixture and volume == volumes[0]:
                    (Path(fixture) / "spool" / "00000000-0000-0000-0000-000000000001.capture").write_bytes(contents)
            if fixture:
                keys = Path(fixture) / "keys"
                keys.mkdir(mode=0o700)
                restored_key = subprocess.run(["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", key_mount, "--entrypoint", "cat", "postgres:17-alpine", f"/keys/{self.old_fingerprint.hex()}.key"], capture_output=True, check=True).stdout
                key_file = keys / f"{self.old_fingerprint.hex()}.key"
                key_file.write_bytes(restored_key)
                key_file.chmod(0o600)
            again = subprocess.run(command, env=self.env, capture_output=True, text=True, timeout=60)
            self.assertNotEqual(again.returncode, 0)
            unchanged = subprocess.run(["docker", "exec", target, "psql", "-U", "gateway", "-d", "gateway", "-Atqc", "SELECT encode(value, 'hex') FROM capture_restore_fixture"], capture_output=True, text=True, check=True).stdout.strip()
            self.assertEqual(unchanged, "01020304")
        finally:
            if started:
                subprocess.run(["docker", "rm", "-f", target], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            for volume in created_volumes:
                subprocess.run(["docker", "volume", "rm", volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)


if __name__ == "__main__":
    unittest.main()
