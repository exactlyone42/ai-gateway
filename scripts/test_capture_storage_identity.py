import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from capture_storage_identity import StorageIdentityError, require_local_docker, storage_identity


class CaptureStorageIdentityTest(unittest.TestCase):
    def mounts(self):
        return [{"Type": "volume", "Name": f"volume-{index}", "Destination": f"/capture-{index}"} for index in range(5)]

    def volumes(self, created="first"):
        return [{"Name": f"volume-{index}", "Driver": "local", "Scope": "local", "CreatedAt": created, "Options": {"password": "fixture-secret"}} for index in range(5)]

    @patch("capture_storage_identity.require_local_docker")
    @patch("capture_storage_identity.command")
    def test_volume_metadata_changes_identity_without_exposing_options(self, command, local):
        command.return_value = json.dumps(self.volumes()).encode()
        original = storage_identity(self.mounts())
        self.assertEqual(storage_identity(self.mounts()), original)
        self.assertNotIn("fixture-secret", original)
        changed_options = self.volumes()
        changed_options[0]["Options"] = {"password": "different-fixture-secret"}
        command.return_value = json.dumps(changed_options).encode()
        self.assertEqual(storage_identity(self.mounts()), original)
        command.return_value = json.dumps(self.volumes("replacement")).encode()
        self.assertNotEqual(storage_identity(self.mounts()), original)
        command.return_value = json.dumps(self.volumes()[:-1]).encode()
        with self.assertRaises(StorageIdentityError):
            storage_identity(self.mounts())

    @patch("capture_storage_identity.require_local_docker")
    def test_bind_directory_is_rejected_without_modifying_it(self, local):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            paths = [root / str(index) for index in range(5)]
            for path in paths:
                path.mkdir()
            mounts = [{"Type": "bind", "Source": str(path), "Destination": f"/capture-{index}"} for index, path in enumerate(paths)]
            with self.assertRaises(StorageIdentityError):
                storage_identity(mounts)
            self.assertEqual(sorted(item.name for item in root.iterdir()), ["0", "1", "2", "3", "4"])

    @patch.dict("os.environ", {"DOCKER_HOST": "tcp://example.invalid:2375"}, clear=True)
    def test_remote_docker_is_rejected(self):
        with self.assertRaises(StorageIdentityError):
            require_local_docker()

    @patch.dict("os.environ", {"DOCKER_CONTEXT": "default", "DOCKER_HOST": "tcp://127.0.0.1:1"}, clear=True)
    @patch("capture_storage_identity.command")
    def test_remote_host_is_rejected_even_with_local_context(self, command):
        command.side_effect = [b"default\n", b'[{"Endpoints":{"docker":{"Host":"unix:///var/run/docker.sock"}}}]']
        with self.assertRaises(StorageIdentityError):
            require_local_docker()


if __name__ == "__main__":
    unittest.main()
