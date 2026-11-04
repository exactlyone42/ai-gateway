import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from capture_backup import BackupError
from verify_legacy_backup_source import verify


class LegacyBackupSourceTest(unittest.TestCase):
    def test_cli_checks_inspected_container_and_rejects_wrong_database(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            docker = root / "docker"
            docker.write_text('''#!/usr/bin/python3
import json, os, sys
args = sys.argv[1:]
if args[0] == "inspect" and args[1] == "gateway-container":
    print(json.dumps([{"Id":"a"*64, "State":{"Running":True,"StartedAt":"2026-11-03T00:00:00Z"}, "Mounts":[{"Type":"volume", "Name":"fixture-spool", "Destination":"/var/lib/agent-pool-gateway/ledger-spool"}], "Config":{"Env":["LEDGER_SPOOL_DIR=/var/lib/agent-pool-gateway/ledger-spool", "DATABASE_URL=postgres://" + os.environ["TEST_DB_USER"] + ":synthetic@database-container:5432/" + os.environ["TEST_DATABASE"]]}, "NetworkSettings":{"Networks":{"fixture":{}}}}]))
elif args[0] == "inspect" and args[1] == "database-container":
    print(json.dumps([{"Id":"b"*64, "Name":"/database-container", "State":{"Running":True,"StartedAt":"2026-11-03T00:00:00Z"}, "Config":{"Env":[]}, "NetworkSettings":{"Networks":{"fixture":{"Aliases":["database-container"],"IPAddress":"172.18.0.2","GlobalIPv6Address":""}}}}]))
elif args[0] == "exec" and "getent" in args:
    print("172.18.0.2 STREAM database-container")
else:
    sys.exit(1)
''')
            docker.chmod(0o755)
            env = os.environ.copy()
            env.update(PATH=f"{root}:{env['PATH']}", TEST_DATABASE="gateway", TEST_DB_USER="gateway")
            command = [sys.executable, str(Path(__file__).with_name("verify_legacy_backup_source.py")), "--gateway", "gateway-container", "--postgres", "database-container", "--volume", "fixture-spool"]
            accepted = subprocess.run(command, env=env, capture_output=True, text=True)
            self.assertEqual(accepted.returncode, 0, accepted.stderr)
            self.assertEqual(accepted.stdout.strip(), f"{'a' * 64} {'b' * 64} 2026-11-03T00:00:00Z 2026-11-03T00:00:00Z 5432")
            self.assertNotIn("synthetic", accepted.stdout)
            env["TEST_DATABASE"] = "gateway_jev_local"
            rejected = subprocess.run(command, env=env, capture_output=True, text=True)
            self.assertNotEqual(rejected.returncode, 0)
            self.assertNotIn("synthetic", rejected.stderr)
            env.update(TEST_DATABASE="gateway", TEST_DB_USER="other")
            rejected_user = subprocess.run(command, env=env, capture_output=True, text=True)
            self.assertNotEqual(rejected_user.returncode, 0)
            self.assertNotIn("synthetic", rejected_user.stderr)

    @patch("verify_legacy_backup_source.database_target")
    @patch("verify_legacy_backup_source.inspect_mounts")
    def test_requires_gateway_database_and_user(self, inspect, database):
        gateway = {"Id": "a" * 64, "State": {"StartedAt": "2026-11-03T00:00:00Z"}, "Config": {"Env": ["LEDGER_SPOOL_DIR=/var/lib/agent-pool-gateway/ledger-spool"]}}
        inspect.return_value = (gateway, [{"Type": "volume", "Name": "fixture-spool"}])
        database.return_value = ("gateway", "gateway", "b" * 64, "2026-11-03T00:00:00Z", 5432)
        verify("gateway-container", "database-container", "fixture-spool")
        database.assert_called_with(gateway, "a" * 64, "database-container")
        for user, name in (("gateway", "gateway_jev_local"), ("other", "gateway")):
            with self.subTest(user=user, name=name):
                database.return_value = (user, name, "b" * 64, "2026-11-03T00:00:00Z", 5432)
                with self.assertRaises(BackupError):
                    verify("gateway-container", "database-container", "fixture-spool")

    @patch("verify_legacy_backup_source.database_target", side_effect=BackupError("source mismatch"))
    @patch("verify_legacy_backup_source.inspect_mounts")
    def test_propagates_network_identity_failure(self, inspect, database):
        inspect.return_value = ({"Id": "gateway-id", "Config": {"Env": ["LEDGER_SPOOL_DIR=/var/lib/agent-pool-gateway/ledger-spool"]}}, [{"Type": "volume", "Name": "fixture-spool"}])
        with self.assertRaises(BackupError):
            verify("gateway-container", "database-container", "fixture-spool")

    @patch("verify_legacy_backup_source.database_target")
    @patch("verify_legacy_backup_source.inspect_mounts")
    def test_rejects_unset_or_different_ledger_path(self, inspect, database):
        for env in ([], ["LEDGER_SPOOL_DIR=/different"], ["LEDGER_SPOOL_DIR=/var/lib/agent-pool-gateway/ledger-spool"] * 2):
            with self.subTest(env=env):
                inspect.return_value = ({"Id": "gateway-id", "Config": {"Env": env}}, [{"Type": "volume", "Name": "fixture-spool"}])
                with self.assertRaises(BackupError):
                    verify("gateway-container", "database-container", "fixture-spool")
        database.assert_not_called()

    @patch("verify_legacy_backup_source.database_target")
    @patch("verify_legacy_backup_source.inspect_mounts")
    def test_rejects_invalid_container_identity_or_start_time(self, inspect, database):
        gateway = {"Id": "a" * 64, "State": {"StartedAt": "2026-11-03T00:00:00Z"}, "Config": {"Env": ["LEDGER_SPOOL_DIR=/var/lib/agent-pool-gateway/ledger-spool"]}}
        inspect.return_value = (gateway, [{"Type": "volume", "Name": "fixture-spool"}])
        for identifier, started in (("invalid", "2026-11-03T00:00:00Z"), ("b" * 64, "invalid time")):
            with self.subTest(identifier=identifier, started=started):
                database.return_value = ("gateway", "gateway", identifier, started, 5432)
                with self.assertRaises(BackupError):
                    verify("gateway-container", "database-container", "fixture-spool")


if __name__ == "__main__":
    unittest.main()
