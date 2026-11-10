from datetime import datetime, timedelta
import os
from pathlib import Path
import subprocess
import time
import unittest
import uuid


ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "tests/fixtures/erasure-witness"
INSTALL = "00000000-0000-4000-8000-000000000001"
OTHER_INSTALL = "00000000-0000-4000-8000-000000000002"
INITIAL = "01" * 32
FIRST = "02" * 32
SECOND = "03" * 32


@unittest.skipUnless(os.environ.get("RUN_ERASURE_WITNESS_DOCKER") == "1", "需要隔離 Docker 驗收")
class ErasureWitnessPostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if os.environ.get("DOCKER_HOST"):
            raise RuntimeError("隔離測試不支援遠端 Docker")
        context = subprocess.check_output(["docker", "context", "show"], text=True).strip()
        endpoint = subprocess.check_output(["docker", "context", "inspect", context, "--format", "{{.Endpoints.docker.Host}}"], text=True).strip()
        if not endpoint.startswith("unix:///"):
            raise RuntimeError("隔離測試需要本機 Docker socket")
        cls.env = {**os.environ, "DOCKER_CONTEXT": context}
        suffix = uuid.uuid4().hex[:12]
        cls.container = "capture-witness-test-" + suffix
        cls.volume = "capture-witness-test-" + suffix
        cls.expiry = "expire=" + (datetime.now() + timedelta(days=1)).date().isoformat()
        cls.created_volume = False
        cls.started = False
        try:
            cls.docker("volume", "create", "--label", cls.expiry, cls.volume)
            cls.created_volume = True
            cls.start_database()
            cls.execute(FIXTURES / "create-role.sql")
            cls.execute(ROOT / "deploy/erasure-witness/schema.sql", witness_owner="gateway_witness_schema")
            cls.execute(ROOT / "deploy/erasure-witness/grant.sql", witness_role="gateway_witness_test")
            cls.execute(FIXTURES / "extra-privileges.sql")
        except BaseException:
            cls.cleanup_resources()
            raise

    @classmethod
    def docker(cls, *arguments, check=True):
        return subprocess.run(["docker", *arguments], env=cls.env, capture_output=True, text=True, timeout=30, check=check)

    @classmethod
    def start_database(cls):
        cls.docker("run", "-d", "--rm", "--pull=never", "--label", cls.expiry, "--name", cls.container, "--network", "none", "--mount", f"type=volume,source={cls.volume},target=/var/lib/postgresql/data", "-e", "POSTGRES_USER=witness_owner", "-e", "POSTGRES_PASSWORD=synthetic-only", "-e", "POSTGRES_DB=witness", "postgres:17-alpine")
        cls.started = True
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            ready = subprocess.run(cls.command(host="127.0.0.1"), input=(FIXTURES / "ready.sql").read_bytes(), env=cls.env, capture_output=True, timeout=5)
            if ready.returncode == 0:
                return
            time.sleep(0.1)
        raise RuntimeError("隔離見證資料庫沒有啟動")

    @classmethod
    def command(cls, database="witness", host=None, **variables):
        options = sum((["-v", name + "=" + str(value)] for name, value in variables.items()), [])
        host_options = ["-h", host] if host else []
        environment = ["-e", "PGPASSWORD=synthetic-only"] if host else []
        return ["docker", "exec", "-i", *environment, cls.container, "psql", "-X", *host_options, "-U", "witness_owner", "-d", database, "-Atq", "-v", "ON_ERROR_STOP=1", *options, "-f", "-"]

    @classmethod
    def execute(cls, source, check=True, database="witness", **variables):
        result = subprocess.run(cls.command(database=database, **variables), input=source.read_bytes(), env=cls.env, capture_output=True, timeout=30)
        if check and result.returncode:
            raise RuntimeError(f"{source.name}: {result.stderr.decode(errors='replace')}")
        return result

    @classmethod
    def cleanup_resources(cls):
        failures = []
        if cls.started:
            try:
                removed = cls.docker("rm", "-f", "-v", cls.container, check=False)
                if removed.returncode and "No such container" not in removed.stderr:
                    failures.append("container")
            except (OSError, subprocess.SubprocessError):
                failures.append("container")
            cls.started = False
        if cls.created_volume:
            try:
                removed = cls.docker("volume", "rm", cls.volume, check=False)
                if removed.returncode:
                    failures.append("volume")
            except (OSError, subprocess.SubprocessError):
                failures.append("volume")
            cls.created_volume = False
        if failures:
            raise RuntimeError("隔離資源清理失敗: " + ", ".join(failures))

    @classmethod
    def tearDownClass(cls):
        cls.cleanup_resources()

    def setUp(self):
        self.execute(FIXTURES / "reset.sql")
        self.execute(ROOT / "deploy/erasure-witness/bootstrap.sql", install=INSTALL, sequence=0, tip=INITIAL)

    def advance(self, install=INSTALL, sequence=0, tip=INITIAL, next_tip=FIRST):
        return self.execute(FIXTURES / "advance.sql", install=install, sequence=sequence, tip=tip, next_tip=next_tip).stdout.strip()

    def position(self):
        return self.execute(FIXTURES / "read.sql").stdout.strip().decode("ascii")

    def caller_safe(self, role="gateway_witness_test"):
        sources = [FIXTURES / "check-role.sql", ROOT / "queries/erasure_witness_caller_safe.sql", FIXTURES / "check-reset.sql"]
        result = subprocess.run(self.command(test_role=role), input=b"\n".join(path.read_bytes() for path in sources), env=self.env, capture_output=True, timeout=30, check=True)
        return result.stdout.strip()

    def test_only_exact_previous_position_can_advance(self):
        self.assertEqual(self.advance(install=OTHER_INSTALL), b"f")
        self.assertEqual(self.advance(sequence=1), b"f")
        self.assertEqual(self.advance(tip=SECOND), b"f")
        self.assertEqual(self.advance(next_tip="02" * 31), b"f")
        self.assertEqual(self.position(), f"{INSTALL}|0|{INITIAL}")
        self.assertEqual(self.advance(), b"t")
        self.assertEqual(self.advance(), b"f")
        self.assertEqual(self.advance(next_tip=SECOND), b"f")
        self.assertEqual(self.position(), f"{INSTALL}|1|{FIRST}")

    def test_role_cannot_modify_or_recreate_position(self):
        for fixture in ("forbidden-update.sql", "forbidden-insert.sql", "forbidden-delete.sql", "forbidden-truncate.sql", "forbidden-ungranted.sql"):
            result = self.execute(FIXTURES / fixture, check=False, install=INSTALL, sequence=0, tip=INITIAL, next_tip=FIRST)
            self.assertNotEqual(result.returncode, 0, fixture)
            self.assertIn(b"permission denied", result.stderr)
        self.assertEqual(self.position(), f"{INSTALL}|0|{INITIAL}")
        self.execute(FIXTURES / "reset.sql")
        result = self.execute(FIXTURES / "forbidden-insert.sql", check=False, install=INSTALL, tip=INITIAL)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"permission denied", result.stderr)
        self.assertEqual(self.position(), "")

    def test_privileged_owner_and_inherited_writer_are_rejected(self):
        schema = ROOT / "deploy/erasure-witness/schema.sql"
        for role in ("witness_owner", "gateway_witness_bad_owner", "gateway_witness_schema"):
            invalid_owner = self.execute(schema, check=False, witness_owner=role)
            self.assertNotEqual(invalid_owner.returncode, 0, role)
            self.assertIn("無登入權限的非特權角色".encode(), invalid_owner.stderr)
        self.execute(FIXTURES / "server-program.sql")
        grant = ROOT / "deploy/erasure-witness/grant.sql"
        for role in ("witness_owner", "gateway_witness_schema", "gateway_witness_inherited", "gateway_witness_excess", "gateway_witness_createrole", "gateway_witness_replication", "gateway_witness_bypass", "gateway_witness_trigger", "gateway_witness_column", "gateway_witness_maintain", "gateway_witness_reference", "gateway_witness_create", "gateway_witness_server_program", "gateway_witness_read_files", "gateway_witness_write_files", "gateway_witness_read_all", "gateway_witness_signal"):
            rejected = self.execute(grant, check=False, witness_role=role)
            self.assertNotEqual(rejected.returncode, 0, role)
            self.assertIn("見證使用者角色具有多餘權限".encode(), rejected.stderr)
        for role in ("gateway_witness_trigger", "gateway_witness_column", "gateway_witness_maintain", "gateway_witness_reference", "gateway_witness_create"):
            rejected = self.execute(FIXTURES / "forbidden-overprivileged.sql", check=False, test_role=role, install=INSTALL, sequence=0, tip=INITIAL, next_tip=FIRST)
            self.assertNotEqual(rejected.returncode, 0, role)
            self.assertIn(b"permission denied", rejected.stderr)
        excess = self.execute(FIXTURES / "forbidden-excess.sql", check=False, install=INSTALL, sequence=0, tip=INITIAL, next_tip=FIRST)
        self.assertNotEqual(excess.returncode, 0)
        self.assertIn(b"permission denied", excess.stderr)
        self.assertEqual(self.position(), f"{INSTALL}|0|{INITIAL}")

    def test_bootstrap_rejects_existing_and_invalid_positions(self):
        bootstrap = ROOT / "deploy/erasure-witness/bootstrap.sql"
        duplicate = self.execute(bootstrap, check=False, install=INSTALL, sequence=0, tip=SECOND)
        self.assertNotEqual(duplicate.returncode, 0)
        self.assertEqual(self.position(), f"{INSTALL}|0|{INITIAL}")
        self.execute(FIXTURES / "reset.sql")
        for sequence, tip in ((-1, INITIAL), (0, "01" * 31)):
            rejected = self.execute(bootstrap, check=False, install=INSTALL, sequence=sequence, tip=tip)
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn(b"check constraint", rejected.stderr)
            self.assertEqual(self.position(), "")

    def test_database_owner_is_not_a_witness_owner_or_writer(self):
        self.execute(FIXTURES / "owned-database.sql")
        try:
            schema = ROOT / "deploy/erasure-witness/schema.sql"
            rejected = self.execute(schema, check=False, database="witness_owned", witness_owner="gateway_witness_database_owner")
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn("無登入權限的非特權角色".encode(), rejected.stderr)
            self.execute(schema, database="witness_owned", witness_owner="gateway_witness_dirty_owner")
            grant = self.execute(ROOT / "deploy/erasure-witness/grant.sql", check=False, database="witness_owned", witness_role="gateway_witness_database_owner")
            self.assertNotEqual(grant.returncode, 0)
            self.assertIn("見證使用者角色具有多餘權限".encode(), grant.stderr)
        finally:
            self.docker("exec", self.container, "dropdb", "-U", "witness_owner", "witness_owned")

    def test_privileged_functions_are_detected_even_after_grant(self):
        self.assertEqual(self.advance(), b"t")
        self.assertEqual(self.caller_safe(), b"t")
        self.assertEqual(self.execute(FIXTURES / "accessible-definers.sql").stdout.strip(), b"0")
        self.execute(FIXTURES / "unsafe-function-create.sql")
        try:
            self.assertEqual(self.execute(FIXTURES / "accessible-definers.sql").stdout.strip(), b"1")
            self.assertEqual(self.caller_safe(), b"f")
            rejected = self.execute(ROOT / "deploy/erasure-witness/grant.sql", check=False, witness_role="gateway_witness_test")
            self.assertNotEqual(rejected.returncode, 0)
            self.execute(FIXTURES / "unsafe-function-call.sql")
            self.assertEqual(self.position(), f"{INSTALL}|0|{INITIAL}")
        finally:
            self.execute(FIXTURES / "unsafe-function-drop.sql")
        self.assertEqual(self.caller_safe(), b"t")

    def test_caller_detects_later_write_grant(self):
        self.assertEqual(self.caller_safe(), b"t")
        self.execute(FIXTURES / "grant-write.sql")
        try:
            self.assertEqual(self.caller_safe(), b"f")
        finally:
            self.execute(FIXTURES / "revoke-write.sql")
        self.assertEqual(self.caller_safe(), b"t")

    def test_default_privileges_cannot_expose_new_witness(self):
        for fixture in ("dirty-owner.sql", "dirty-owner-function.sql", "dirty-owner-schema.sql"):
            self.docker("exec", self.container, "createdb", "-U", "witness_owner", "witness_dirty")
            try:
                self.execute(FIXTURES / fixture, database="witness_dirty")
                rejected = self.execute(ROOT / "deploy/erasure-witness/schema.sql", check=False, database="witness_dirty", witness_owner="gateway_witness_dirty_owner")
                self.assertNotEqual(rejected.returncode, 0, fixture)
                self.assertIn("見證結構包含多餘授權".encode(), rejected.stderr)
                self.assertEqual(self.execute(FIXTURES / "schema-count.sql", database="witness_dirty").stdout.strip(), b"0")
            finally:
                self.docker("exec", self.container, "dropdb", "-U", "witness_owner", "witness_dirty")

    def test_concurrent_updates_cannot_both_advance(self):
        source = (FIXTURES / "concurrent.sql").read_bytes()
        clients = [subprocess.Popen(self.command(install=INSTALL, sequence=0, tip=INITIAL, next_tip=tip), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.env) for tip in (FIRST, SECOND)]
        try:
            for client in clients:
                client.stdin.write(source)
                client.stdin.close()
            waiting = 0
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                waiting = int(self.execute(FIXTURES / "waiting.sql").stdout.strip())
                if waiting:
                    break
                time.sleep(0.05)
            self.assertGreater(waiting, 0)
            results = []
            for client in clients:
                self.assertEqual(client.wait(timeout=10), 0)
                results.append(client.stdout.read().strip())
            self.assertCountEqual(results, [b"t", b"f"])
            self.assertIn(self.position(), (f"{INSTALL}|1|{FIRST}", f"{INSTALL}|1|{SECOND}"))
        finally:
            for client in clients:
                if client.poll() is None:
                    client.kill()
                    client.wait(timeout=10)
                if client.stdin and not client.stdin.closed:
                    client.stdin.close()
                client.stdout.close()
                client.stderr.close()

    def test_temporary_table_cannot_redirect_advance(self):
        result = self.execute(FIXTURES / "temporary-shadow.sql", install=INSTALL, tip=INITIAL, next_tip=FIRST)
        self.assertEqual(result.stdout.splitlines(), [b"t", b"1"])
        self.assertEqual(self.position(), f"{INSTALL}|1|{FIRST}")

    def test_committed_position_survives_database_process_crash(self):
        self.assertEqual(self.execute(FIXTURES / "sync.sql").stdout.strip(), b"on")
        self.assertEqual(self.advance(), b"t")
        self.docker("kill", "-s", "KILL", self.container)
        cls = type(self)
        cls.started = False
        cls.container += "-restart"
        cls.start_database()
        self.assertEqual(self.position(), f"{INSTALL}|1|{FIRST}")
        self.assertEqual(self.advance(), b"f")


if __name__ == "__main__":
    unittest.main()
