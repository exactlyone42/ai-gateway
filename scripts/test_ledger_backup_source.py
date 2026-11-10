from pathlib import Path
import contextlib
import io
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from capture_backup import BackupError
from ledger_backup_source import LEDGER_DESTINATION, main, require_logged_relations, require_quiet_database, require_single_volume_owner, source, wal_position


class LedgerBackupSourceTest(unittest.TestCase):
    def test_unlogged_relation_check_fails_closed(self):
        for status, output, accepted in ((0, b"0\n", True), (0, b"1\n", False), (1, b"0\n", False), (0, b"", False)):
            with self.subTest(status=status, output=output), patch("ledger_backup_source.subprocess.run", return_value=SimpleNamespace(returncode=status, stdout=output)):
                if accepted:
                    require_logged_relations("fixture", 5432, "gateway", "gateway")
                else:
                    with self.assertRaises(BackupError):
                        require_logged_relations("fixture", 5432, "gateway", "gateway")

    def test_wal_position_rejects_invalid_or_failed_queries(self):
        for status, output, accepted in ((0, b"0/1A2B\n", True), (0, b"invalid\n", False), (1, b"0/1A2B\n", False), (0, b"", False)):
            with self.subTest(status=status, output=output), patch("ledger_backup_source.subprocess.run", return_value=SimpleNamespace(returncode=status, stdout=output)):
                if accepted:
                    self.assertEqual(wal_position("fixture", 5432, "gateway", "gateway"), b"0/1A2B")
                else:
                    with self.assertRaises(BackupError):
                        wal_position("fixture", 5432, "gateway", "gateway")

    def test_volume_owner_check_rejects_extra_or_unknown_containers(self):
        owner = "a" * 64
        other = "b" * 64
        mount = '[{"Type":"volume","Name":"fixture-volume"}]'
        for status, listing, details, accepted in ((0, owner + "\n", owner + " " + mount + "\n", True), (0, owner + "\n" + other + "\n", owner + " " + mount + "\n" + other + " " + mount + "\n", False), (0, "", "", False), (1, owner + "\n", "", False), (0, owner + "\n", "invalid\n", False)):
            responses = [SimpleNamespace(returncode=status, stdout=listing.encode()), SimpleNamespace(returncode=0, stdout=details.encode())]
            with self.subTest(status=status, listing=listing, details=details), patch("ledger_backup_source.subprocess.run", side_effect=responses):
                if accepted:
                    require_single_volume_owner("fixture-volume", owner)
                else:
                    with self.assertRaises(BackupError):
                        require_single_volume_owner("fixture-volume", owner)

    def test_quiet_database_rejects_other_sessions_and_query_failures(self):
        for status, output, accepted in ((0, b"0\n", True), (0, b"1\n", False), (1, b"0\n", False), (0, b"", False)):
            with self.subTest(status=status, output=output), patch("ledger_backup_source.subprocess.run", return_value=SimpleNamespace(returncode=status, stdout=output)):
                if accepted:
                    require_quiet_database("fixture", 5432, "gateway", "gateway")
                else:
                    with self.assertRaises(BackupError):
                        require_quiet_database("fixture", 5432, "gateway", "gateway")

    @patch("ledger_backup_source.source", return_value={"capture_setting": "unconfirmed", "capture_disabled_setting_explicit": False})
    def test_unconfirmed_inspection_exits_nonzero(self, mocked_source):
        with patch.object(sys, "argv", ["ledger_backup_source.py", "--gateway", "gateway", "--postgres", "postgres", "--inspect-unconfirmed"]), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(main(), 2)

    def gateway(self, capture=None):
        environment = ["LEDGER_SPOOL_DIR=" + LEDGER_DESTINATION, "DATABASE_URL=postgres://gateway:fixture-secret@database:5432/actual"]
        if capture is not None:
            environment.append("CONTENT_CAPTURE_ENABLED=" + capture)
        return {"Id": "a" * 64, "State": {"StartedAt": "stable"}, "Config": {"Env": environment}}

    @patch("ledger_backup_source.require_local_docker")
    @patch("ledger_backup_source.require_same_instance")
    @patch("ledger_backup_source.storage_identity", return_value="c" * 64)
    @patch("ledger_backup_source.subprocess.run")
    @patch("ledger_backup_source.database_identity", return_value=("1234", "123"))
    @patch("ledger_backup_source.database_target", return_value=("gateway", "actual", "b" * 64, "stable", 5432))
    @patch("ledger_backup_source.inspect_mounts")
    def test_disabled_or_unset_capture_is_reported_without_secrets(self, inspect, target, identity, run, storage, same, local):
        run.return_value = SimpleNamespace(returncode=0, stdout=b"f\n")
        for setting, expected in (("false", "disabled"), (None, "unconfirmed")):
            with self.subTest(setting=setting):
                inspect.return_value = (self.gateway(setting), [{"Type": "volume", "Name": "fixture"}])
                result = source("gateway", "postgres", inspect_unconfirmed=setting is None)
                self.assertEqual(result["capture_setting"], expected)
                self.assertEqual(result["capture_disabled_setting_explicit"], setting == "false")
                self.assertEqual(result["postgres_id"], "b" * 64)
                self.assertNotIn("fixture-secret", str(result))
        self.assertEqual(storage.call_args.kwargs["expected"], 1)

    @patch("ledger_backup_source.require_local_docker")
    @patch("ledger_backup_source.database_target")
    @patch("ledger_backup_source.inspect_mounts")
    def test_enabled_or_ambiguous_capture_is_rejected_before_database_query(self, inspect, target, local):
        for setting in ("true", "TRUE", "invalid"):
            with self.subTest(setting=setting):
                inspect.return_value = (self.gateway(setting), [{"Type": "volume", "Name": "fixture"}])
                with self.assertRaises(BackupError):
                    source("gateway", "postgres")
        target.assert_not_called()

    @patch("ledger_backup_source.require_local_docker")
    @patch("ledger_backup_source.subprocess.run", return_value=SimpleNamespace(returncode=0, stdout=b"t\n"))
    @patch("ledger_backup_source.database_identity", return_value=("1234", "123"))
    @patch("ledger_backup_source.database_target", return_value=("gateway", "actual", "b" * 64, "stable", 5432))
    @patch("ledger_backup_source.inspect_mounts")
    def test_capture_tables_are_rejected(self, inspect, target, identity, run, local):
        inspect.return_value = (self.gateway("false"), [{"Type": "volume", "Name": "fixture"}])
        with self.assertRaises(BackupError):
            source("gateway", "postgres")

    @patch("ledger_backup_source.require_local_docker")
    @patch("ledger_backup_source.require_same_instance")
    @patch("ledger_backup_source.storage_identity", side_effect=["c" * 64, "d" * 64])
    @patch("ledger_backup_source.subprocess.run", return_value=SimpleNamespace(returncode=0, stdout=b"f\n"))
    @patch("ledger_backup_source.database_identity", return_value=("1234", "123"))
    @patch("ledger_backup_source.database_target", return_value=("gateway", "actual", "b" * 64, "stable", 5432))
    @patch("ledger_backup_source.inspect_mounts")
    def test_changed_storage_is_rejected(self, inspect, target, identity, run, storage, same, local):
        inspect.return_value = (self.gateway("false"), [{"Type": "volume", "Name": "fixture"}])
        with self.assertRaises(BackupError):
            source("gateway", "postgres")

    @patch("ledger_backup_source.require_local_docker")
    @patch("ledger_backup_source.require_same_instance")
    @patch("ledger_backup_source.storage_identity", return_value="c" * 64)
    @patch("ledger_backup_source.subprocess.run", side_effect=[SimpleNamespace(returncode=0, stdout=b"f\n"), SimpleNamespace(returncode=0, stdout=b"t\n")])
    @patch("ledger_backup_source.database_identity", return_value=("1234", "123"))
    @patch("ledger_backup_source.database_target", return_value=("gateway", "actual", "b" * 64, "stable", 5432))
    @patch("ledger_backup_source.inspect_mounts")
    def test_capture_tables_appearing_during_check_are_rejected(self, inspect, target, identity, run, storage, same, local):
        inspect.return_value = (self.gateway("false"), [{"Type": "volume", "Name": "fixture"}])
        with self.assertRaises(BackupError):
            source("gateway", "postgres")

    @patch("ledger_backup_source.require_local_docker")
    @patch("ledger_backup_source.require_same_instance")
    @patch("ledger_backup_source.storage_identity", return_value="c" * 64)
    @patch("ledger_backup_source.subprocess.run", return_value=SimpleNamespace(returncode=0, stdout=b"f\n"))
    @patch("ledger_backup_source.database_identity", return_value=("1234", "123"))
    @patch("ledger_backup_source.database_target", return_value=("gateway", "actual", "b" * 64, "stable", 5432))
    @patch("ledger_backup_source.inspect_mounts")
    def test_unset_capture_is_rejected_by_default_before_database_query(self, inspect, target, identity, run, storage, same, local):
        inspect.return_value = (self.gateway(), [{"Type": "volume", "Name": "fixture"}])
        with self.assertRaises(BackupError):
            source("gateway", "postgres")
        target.assert_not_called()

    @patch("ledger_backup_source.require_local_docker")
    @patch("ledger_backup_source.require_same_instance")
    @patch("ledger_backup_source.storage_identity", return_value="c" * 64)
    @patch("ledger_backup_source.subprocess.run", return_value=SimpleNamespace(returncode=0, stdout=b"f\n"))
    @patch("ledger_backup_source.database_identity", return_value=("1234", "123"))
    @patch("ledger_backup_source.database_target", return_value=("gateway", "actual", "b" * 64, "stable", 5432))
    @patch("ledger_backup_source.inspect_mounts")
    def test_gateway_restart_between_checks_is_rejected(self, inspect, target, identity, run, storage, same, local):
        before = self.gateway("false")
        after = self.gateway("false")
        after["State"]["StartedAt"] = "restarted"
        mounts = [{"Type": "volume", "Name": "fixture"}]
        inspect.side_effect = [(before, mounts), (after, mounts)]
        with self.assertRaises(BackupError):
            source("gateway", "postgres")


if __name__ == "__main__":
    unittest.main()
