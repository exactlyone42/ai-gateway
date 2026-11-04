import os
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
from restore_capture_backup import RestoreError, preflight, require_same_target, stage_backup
from verify_capture_backup import FILES, LEDGER_FILES
from capture_postgres import configured_port, local_exec


class CaptureRestoreStagingTest(unittest.TestCase):
    def test_ledger_staging_copies_only_required_files_and_is_bounded(self):
        with tempfile.TemporaryDirectory(prefix="ledger-stage-test-") as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            names = LEDGER_FILES | {"manifest.json", "manifest.sig", ".complete"}
            for name in names:
                item = source / name
                item.write_bytes(b"fixture")
                item.chmod(0o600)
            target = root / "target"
            target.mkdir()
            stage_backup(source, target, len(names) * 7, LEDGER_FILES)
            self.assertEqual({item.name for item in target.iterdir()}, names)
            with self.assertRaises(RestoreError):
                stage_backup(source, root / "unused", 1, LEDGER_FILES)

    def test_restore_volume_count_must_match_explicit_profile(self):
        for volumes, expected in (([], 1), (["a", "a"], 1), (["a"], 4), (["a"], 2)):
            with self.subTest(volumes=volumes, expected=expected), self.assertRaises(RestoreError):
                preflight("db", "gateway", "gateway", volumes, expected)
        target = {"Id": "fixture", "State": {"Running": True, "StartedAt": "first"}, "HostConfig": {"NetworkMode": "none"}, "Config": {"Env": []}}
        def output(command, input_data=None):
            if command[:2] == ["docker", "inspect"]:
                return json.dumps([target]).encode()
            if command[:3] == ["docker", "volume", "inspect"]:
                return json.dumps([{"Name": "ledger"}]).encode()
            if command[:3] == ["docker", "ps", "-aq"]:
                return b""
            if command[:2] == ["docker", "run"]:
                return b""
            return b"0"
        with mock.patch("restore_capture_backup.output", side_effect=output):
            self.assertEqual(preflight("db", "gateway", "gateway", ["ledger"], 1), ("fixture", "first", 5432))

    def test_local_database_command_clears_redirecting_environment(self):
        for program in ("psql", "pg_dump", "pg_restore"):
            command = local_exec("fixture-container", 6432, program)
            for name in ("PGHOST", "PGHOSTADDR", "PGPORT", "PGSERVICE", "PGSERVICEFILE", "PGDATABASE", "PGUSER", "PGOPTIONS"):
                self.assertIn(["-u", name], [command[index:index + 2] for index in range(len(command) - 1)])
            self.assertIn(["-h", "/var/run/postgresql"], [command[index:index + 2] for index in range(len(command) - 1)])
            self.assertIn(["-p", "6432"], [command[index:index + 2] for index in range(len(command) - 1)])
            self.assertEqual("-X" in command, program == "psql")
        self.assertEqual(configured_port({"Config": {"Env": []}}), 5432)
        self.assertEqual(configured_port({"Config": {"Env": ["PGPORT=6432"]}}), 6432)
        for values in (["PGPORT=0"], ["PGPORT=65536"], ["PGPORT=abc"], ["PGPORT=1", "PGPORT=2"]):
            with self.assertRaises(ValueError):
                configured_port({"Config": {"Env": values}})

    def test_target_must_remain_the_same_running_isolated_instance(self):
        original = {"Id": "fixture", "State": {"Running": True, "StartedAt": "first"}, "HostConfig": {"NetworkMode": "none"}}
        variants = [
            {**original, "Id": "other"},
            {**original, "State": {"Running": True, "StartedAt": "second"}},
            {**original, "State": {"Running": False, "StartedAt": "first"}},
            {**original, "HostConfig": {"NetworkMode": "bridge"}},
        ]
        with mock.patch("restore_capture_backup.output", return_value=json.dumps([original]).encode()):
            require_same_target("fixture", "first")
        for changed in variants:
            with mock.patch("restore_capture_backup.output", return_value=json.dumps([changed]).encode()):
                with self.assertRaises(RestoreError):
                    require_same_target("fixture", "first")

    def test_staging_bounds_and_source_size_changes(self):
        with tempfile.TemporaryDirectory(prefix="capture-stage-test-") as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            names = FILES | {"manifest.json", "manifest.sig", ".complete"}
            for name in names:
                item = source / name
                item.write_bytes(b"fixture")
                item.chmod(0o600)
            normal = root / "normal"
            normal.mkdir()
            stage_backup(source, normal, len(names) * len(b"fixture"))
            self.assertEqual({item.name for item in normal.iterdir()}, names)
            self.assertTrue(all((normal / name).read_bytes() == b"fixture" for name in names))
            too_small = root / "too-small"
            too_small.mkdir()
            with self.assertRaises(RestoreError):
                stage_backup(source, too_small, 1)
            self.assertEqual(list(too_small.iterdir()), [])
            changed = root / "changed"
            changed.mkdir()
            original = os.fstat
            def shorter(fd):
                metadata = original(fd)
                return SimpleNamespace(st_mode=metadata.st_mode, st_size=max(0, metadata.st_size - 1))
            with mock.patch("restore_capture_backup.os.fstat", side_effect=shorter):
                with self.assertRaises(RestoreError):
                    stage_backup(source, changed, 1000)
            self.assertTrue(all((source / name).read_bytes() == b"fixture" for name in names))
            growing = root / "growing"
            growing.mkdir()
            first = sorted(names)[0]
            calls = 0
            def grow_after_stat(fd):
                nonlocal calls
                metadata = original(fd)
                calls += 1
                if calls == 2:
                    with (source / first).open("ab") as item:
                        item.write(b"x")
                return metadata
            with mock.patch("restore_capture_backup.os.fstat", side_effect=grow_after_stat):
                with self.assertRaises(RestoreError):
                    stage_backup(source, growing, 1000)
            self.assertEqual((source / first).read_bytes(), b"fixturex")
            self.assertEqual((growing / first).stat().st_size, 0)


if __name__ == "__main__":
    unittest.main()
