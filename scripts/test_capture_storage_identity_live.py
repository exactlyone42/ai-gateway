from datetime import date, timedelta
import os
from pathlib import Path
import subprocess
import sys
import time
import unittest
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from capture_storage_identity import storage_identity


@unittest.skipUnless(os.environ.get("RUN_CAPTURE_STORAGE_DOCKER") == "1", "requires isolated Docker volumes")
class CaptureStorageIdentityLiveTest(unittest.TestCase):
    def test_recreated_named_volume_changes_identity(self):
        run_id = uuid.uuid4().hex
        label = "gateway.storage_identity_test=" + run_id
        names = [f"gateway-storage-identity-{run_id}-{index}" for index in range(5)]
        expiry = "expire=" + (date.today() + timedelta(days=1)).isoformat()
        try:
            for name in names:
                subprocess.run(["docker", "volume", "create", "--label", expiry, "--label", label, name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, check=True)
            mounts = [{"Type": "volume", "Name": name, "Destination": f"/capture-{index}"} for index, name in enumerate(names)]
            original = storage_identity(mounts)
            self.assertEqual(storage_identity(mounts), original)
            time.sleep(1.1)
            subprocess.run(["docker", "volume", "rm", names[0]], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, check=True)
            subprocess.run(["docker", "volume", "create", "--label", expiry, "--label", label, names[0]], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, check=True)
            self.assertNotEqual(storage_identity(mounts), original)
        finally:
            for name in names:
                inspect = subprocess.run(["docker", "volume", "inspect", "--format", "{{index .Labels \"gateway.storage_identity_test\"}}", name], capture_output=True, text=True, timeout=10)
                if inspect.returncode == 0 and inspect.stdout.strip() == run_id:
                    subprocess.run(["docker", "volume", "rm", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
            remaining = subprocess.run(["docker", "volume", "ls", "-q", "--filter", "label=" + label], capture_output=True, text=True, timeout=10, check=True).stdout.strip()
            if remaining:
                raise RuntimeError("isolated Docker volumes remain")


if __name__ == "__main__":
    unittest.main()
