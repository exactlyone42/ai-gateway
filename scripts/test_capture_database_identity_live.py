from datetime import date, timedelta
import os
from pathlib import Path
import subprocess
import sys
import time
import unittest
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from capture_postgres import database_identity


ROOT = Path(__file__).resolve().parent.parent


@unittest.skipUnless(os.environ.get("RUN_CAPTURE_IDENTITY_DOCKER") == "1", "requires isolated Docker PostgreSQL")
class CaptureDatabaseIdentityLiveTest(unittest.TestCase):
    def test_database_identity_distinguishes_database_and_cluster(self):
        run_id = uuid.uuid4().hex
        label = "gateway.identity_test=" + run_id
        containers = []
        try:
            for index in range(2):
                name = f"gateway-identity-{run_id}-{index}"
                containers.append(name)
                subprocess.run(["docker", "run", "-d", "--rm", "--label", "expire=" + (date.today() + timedelta(days=1)).isoformat(), "--label", label, "--network", "none", "--pull=never", "--name", name, "-e", "POSTGRES_USER=gateway", "-e", "POSTGRES_PASSWORD=fixture-password", "-e", "POSTGRES_DB=identity_first", "postgres:17-alpine"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    ready = subprocess.run(["docker", "exec", name, "pg_isready", "-U", "gateway", "-d", "identity_first"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
                    if ready.returncode == 0:
                        break
                    time.sleep(0.2)
                else:
                    self.fail("isolated PostgreSQL did not become ready")
            with (ROOT / "queries/test_capture_database_identity_create.sql").open("rb") as query:
                subprocess.run(["docker", "exec", "-i", containers[0], "psql", "-X", "-U", "gateway", "-d", "identity_first", "-v", "ON_ERROR_STOP=1"], stdin=query, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=True)
            first = database_identity(containers[0], 5432, "gateway", "identity_first")
            other = database_identity(containers[0], 5432, "gateway", "capture_identity_other")
            second = database_identity(containers[1], 5432, "gateway", "identity_first")
            self.assertEqual(first[0], other[0])
            self.assertNotEqual(first[1], other[1])
            self.assertNotEqual(first[0], second[0])
            with (ROOT / "queries/test_capture_database_identity_permissions.sql").open("rb") as query:
                subprocess.run(["docker", "exec", "-i", containers[0], "psql", "-X", "-U", "gateway", "-d", "identity_first", "-v", "ON_ERROR_STOP=1"], stdin=query, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=True)
            with (ROOT / "queries/test_capture_database_identity_connection.sql").open("rb") as query:
                connection = subprocess.run(["docker", "exec", "-i", containers[0], "psql", "-X", "-U", "capture_identity_reader", "-d", "identity_first", "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=query, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=True)
            self.assertEqual(connection.stdout.strip(), b"1")
            with (ROOT / "queries/capture_backup_database_identity.sql").open("rb") as query:
                denied = subprocess.run(["docker", "exec", "-i", containers[0], "env", "LC_ALL=C", "psql", "-X", "-U", "capture_identity_reader", "-d", "identity_first", "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=query, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
            self.assertNotEqual(denied.returncode, 0)
            self.assertIn(b"permission denied for function pg_control_system", denied.stderr)
            with self.assertRaises(ValueError):
                database_identity(containers[0], 5432, "capture_identity_reader", "identity_first")
            self.assertEqual(database_identity(containers[0], 5432, "gateway", "identity_first"), first)
        finally:
            for name in containers:
                inspect = subprocess.run(["docker", "inspect", "--format", "{{index .Config.Labels \"gateway.identity_test\"}}", name], capture_output=True, text=True, timeout=10)
                if inspect.returncode == 0 and inspect.stdout.strip() == run_id:
                    subprocess.run(["docker", "stop", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
            deadline = time.monotonic() + 10
            remaining = "unknown"
            while time.monotonic() < deadline:
                remaining = subprocess.run(["docker", "ps", "-aq", "--filter", "label=" + label], capture_output=True, text=True, timeout=10, check=True).stdout.strip()
                if not remaining:
                    break
                time.sleep(0.2)
            if remaining:
                raise RuntimeError("isolated PostgreSQL resources remain")


if __name__ == "__main__":
    unittest.main()
