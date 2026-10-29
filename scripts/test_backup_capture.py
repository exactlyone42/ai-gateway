import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("backup.sh")


class BackupCaptureGuardTest(unittest.TestCase):
    def run_operation(self, inventory, existing, inspect_status="0", operation="restore"):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        backup = root / "backup"
        backup.mkdir()
        (backup / "gateway.dump").write_bytes(b"test")
        (backup / "ledger-spool.tgz").write_bytes(b"test")
        log = root / "docker.log"
        docker = root / "docker"
        docker.write_text(
            "#!/bin/sh\n"
            "printf '%s\\n' \"$*\" >> \"$TEST_DOCKER_LOG\"\n"
            "case \"$*\" in\n"
            "  *'pg_restore --list'*) printf '%s\\n' \"$TEST_INVENTORY\"; exit \"$TEST_INSPECT_STATUS\";;\n"
            "  *'psql -U gateway'*) printf '%s\\n' \"$TEST_EXISTING\";;\n"
            "esac\n"
            "exit 0\n"
        )
        docker.chmod(0o755)
        env = os.environ.copy()
        env.update(
            PATH=f"{root}:{env['PATH']}",
            TEST_DOCKER_LOG=str(log),
            TEST_INVENTORY=inventory,
            TEST_EXISTING=existing,
            TEST_INSPECT_STATUS=inspect_status,
        )
        result = subprocess.run(["bash", str(SCRIPT), operation, str(backup)], env=env, capture_output=True, text=True)
        return result, log.read_text()

    def test_capture_in_backup_is_rejected_before_stopping_gateway(self):
        result, log = self.run_operation("123; 1259 456 TABLE public capture_records gateway", "f")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("safe capture restore is not implemented", result.stderr)
        self.assertNotIn("stop ", log)

    def test_capture_in_live_database_is_rejected_before_stopping_gateway(self):
        result, log = self.run_operation("123; 1259 456 TABLE public users gateway", "t")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("safe capture backup and restore are not implemented", result.stderr)
        self.assertNotIn("stop ", log)

    def test_unreadable_backup_is_rejected_before_stopping_gateway(self):
        result, log = self.run_operation("", "f", "1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot inspect backup", result.stderr)
        self.assertNotIn("stop ", log)

    def test_legacy_backup_continues_through_existing_restore_path(self):
        result, log = self.run_operation("123; 1259 456 TABLE public users gateway", "f")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("stop apg-gateway-1", log)
        self.assertIn("pg_restore -U gateway -d gateway --clean", log)

    def test_capture_backup_is_rejected_before_dump(self):
        result, log = self.run_operation("", "t", operation="create")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("safe capture backup and restore are not implemented", result.stderr)
        self.assertNotIn("pg_dump", log)


if __name__ == "__main__":
    unittest.main()
