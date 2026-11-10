from pathlib import Path
from types import SimpleNamespace
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from capture_backup import BackupError
from capture_backup_source_state import state
from capture_postgres import valid_database_identity


class BackupSourceStateTest(unittest.TestCase):
    def test_database_identity_enforces_numeric_ranges(self):
        for system_id in (str(-(2**63)), "-1234", "0", str(2**63 - 1)):
            self.assertTrue(valid_database_identity(system_id, "4294967295"))
        for system_id in (str(-(2**63) - 1), str(2**63), "invalid", ""):
            self.assertFalse(valid_database_identity(system_id, "123"))
        for oid in ("-1", "4294967296", "invalid", ""):
            self.assertFalse(valid_database_identity("1234", oid))

    @patch("capture_backup_source_state.require_same_instance")
    @patch("capture_backup_source_state.storage_identity", return_value="c" * 64)
    @patch("capture_backup_source_state.database_identity", return_value=("1234", "123"))
    @patch("capture_backup_source_state.subprocess.run")
    @patch("capture_backup_source_state.database_target")
    @patch("capture_backup_source_state.inspect_mounts")
    def test_queries_the_resolved_database_and_rechecks_source(self, inspect, target, run, identity, storage, same):
        keys = ("CONTENT_CAPTURE_SPOOL_DIR", "CONTENT_CAPTURE_KEY_DIR", "LEDGER_SPOOL_DIR", "CONTENT_CAPTURE_ERASURE_DIR", "CONTENT_CAPTURE_WITNESS_DIR")
        inspect.return_value = ({"Id": "a" * 64, "State": {"StartedAt": "gateway-start"}, "Config": {"Env": [key + "=/" + key for key in keys]}}, [])
        target.return_value = ("actual-user", "actual-database", "b" * 64, "database-start", 5432)
        run.return_value = SimpleNamespace(returncode=0, stdout=b"t\n")
        self.assertEqual(state("gateway", "postgres"), ("t", "a" * 64, "b" * 64, "1234", "123", "c" * 64))
        command = run.call_args.args[0]
        self.assertEqual(command[command.index("-U") + 1], "actual-user")
        self.assertEqual(command[command.index("-d") + 1], "actual-database")
        self.assertEqual(target.call_count, 2)
        self.assertEqual(same.call_count, 4)
        target.side_effect = [("actual-user", "actual-database", "b" * 64, "database-start", 5432), ("actual-user", "other", "b" * 64, "database-start", 5432)]
        with self.assertRaises(BackupError):
            state("gateway", "postgres")

    @patch("capture_backup_source_state.subprocess.run")
    @patch("capture_backup_source_state.database_target", side_effect=BackupError("wrong database"))
    @patch("capture_backup_source_state.inspect_mounts")
    def test_rejects_unidentified_source_without_querying(self, inspect, target, run):
        inspect.return_value = ({"Id": "a" * 64, "State": {"StartedAt": "gateway-start"}}, [])
        with self.assertRaises(BackupError):
            state("gateway", "postgres")
        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
