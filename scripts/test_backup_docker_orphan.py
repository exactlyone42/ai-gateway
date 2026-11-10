from datetime import date, timedelta
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from capture_backup import BackupError, encrypt_stream
from ledger_backup_source import spool_fingerprint


@unittest.skipUnless(os.environ.get("RUN_BACKUP_ORPHAN_DOCKER") == "1", "requires isolated Docker")
class BackupDockerOrphanTest(unittest.TestCase):
    def test_cancelled_spool_check_removes_its_stalled_container(self):
        nonce = uuid.uuid4().hex
        volume = "gateway-spool-cleanup-" + nonce
        label = "gateway.spool_cleanup_test=" + nonce
        expiry = "expire=" + (date.today() + timedelta(days=1)).isoformat()
        real_docker = shutil.which("docker")
        self.assertIsNotNone(real_docker)
        def owners():
            output = subprocess.run([real_docker, "ps", "-aq", "--no-trunc", "--filter", "label=gateway.backup_producer"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=True).stdout
            return set(output.splitlines())
        before = owners()
        subprocess.run([real_docker, "volume", "create", "--label", expiry, "--label", label, volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
        try:
            with tempfile.TemporaryDirectory(prefix="gateway-owned-spool-") as directory:
                root = Path(directory)
                wrapper = root / "docker"
                fixture = "from pathlib import Path; import os,time; Path('/fixture/started').touch(); os.write(1,b'x'*1024); time.sleep(300)"
                wrapper.write_text("#!" + sys.executable + "\nimport os,sys\na=sys.argv[1:]\nif a and a[0]=='run' and '--entrypoint' in a and a[a.index('--entrypoint')+1]=='tar':\n i=a.index('--entrypoint'); a[i+1]='python'; j=a.index('python:3.13-slim'); a[j:j]=['--mount'," + repr("type=bind,source=" + directory + ",target=/fixture") + "]; j=a.index('python:3.13-slim'); a=a[:j+1]+['-u','-c'," + repr(fixture) + "]\nos.execv(" + repr(real_docker) + ",[" + repr(real_docker) + "]+a)\n")
                wrapper.chmod(0o700)
                lock = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(3)"])
                try:
                    with mock.patch.dict(os.environ, {"PATH": directory + os.pathsep + os.environ.get("PATH", "")}):
                        with self.assertRaises(BackupError):
                            spool_fingerprint(volume, lock)
                    self.assertTrue((root / "started").exists())
                    self.assertEqual(owners(), before)
                finally:
                    if lock.poll() is None:
                        lock.terminate()
                    lock.wait(timeout=10)
        finally:
            owner = subprocess.run([real_docker, "volume", "inspect", "--format", "{{index .Labels \"gateway.spool_cleanup_test\"}}", volume], capture_output=True, text=True, timeout=10)
            if owner.returncode == 0 and owner.stdout.strip() == nonce:
                subprocess.run([real_docker, "volume", "rm", volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)

    def test_cancelled_backup_removes_its_stalled_source_container(self):
        def owners():
            output = subprocess.run(["docker", "ps", "-aq", "--no-trunc", "--filter", "label=gateway.backup_producer"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=True).stdout
            return set(output.splitlines())
        before = owners()
        with tempfile.TemporaryDirectory(prefix="gateway-owned-source-") as directory:
            root = Path(directory)
            fake_gpg = root / "gpg"
            fake_gpg.write_text("#!" + sys.executable + "\nimport time\ntime.sleep(60)\n")
            fake_gpg.chmod(0o700)
            fixture = "from pathlib import Path; import os,time; Path('/fixture/started').touch(); os.write(1,b'x'*1024); time.sleep(300)"
            producer = ["docker", "run", "--rm", "--pull=never", "--network", "none", "--mount", f"type=bind,source={directory},target=/fixture", "python:3.13-slim", "python", "-u", "-c", fixture]
            lock = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(3)"])
            try:
                with mock.patch.dict(os.environ, {"PATH": directory + os.pathsep + os.environ.get("PATH", "")}):
                    with self.assertRaises(BackupError):
                        encrypt_stream(producer, root / "backup.gpg", "a" * 40, lock, None)
                self.assertTrue((root / "started").exists())
                self.assertEqual(owners(), before)
            finally:
                if lock.poll() is None:
                    lock.terminate()
                lock.wait(timeout=10)

    def test_killing_client_does_not_stop_running_container(self):
        nonce = uuid.uuid4().hex
        label = "gateway.backup_orphan_test=" + nonce
        expiry = "expire=" + (date.today() + timedelta(days=1)).isoformat()
        name = "gateway-backup-orphan-" + nonce
        process = None
        try:
            fixture = "import time; print('READY',flush=True); time.sleep(300)"
            process = subprocess.Popen(["docker", "run", "--rm", "--label", expiry, "--label", label, "--pull=never", "--name", name, "--network", "none", "python:3.13-slim", "python", "-u", "-c", fixture], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            self.assertTrue(select.select([process.stdout], [], [], 15)[0])
            self.assertEqual(process.stdout.readline().strip(), b"READY")
            process.kill()
            self.assertEqual(process.wait(timeout=10), -9)
            running = subprocess.run(["docker", "inspect", "--format", "{{.State.Running}}", name], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=True).stdout.strip()
            self.assertEqual(running, b"true")
        finally:
            if process is not None:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)
                process.stdout.close()
            owner = subprocess.run(["docker", "inspect", "--format", "{{index .Config.Labels \"gateway.backup_orphan_test\"}}", name], capture_output=True, text=True, timeout=10)
            if owner.returncode == 0 and owner.stdout.strip() == nonce:
                subprocess.run(["docker", "rm", "-f", "-v", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
            remaining = subprocess.run(["docker", "ps", "-aq", "--filter", "label=" + label], capture_output=True, timeout=10, check=True).stdout.strip()
            self.assertEqual(remaining, b"")


if __name__ == "__main__":
    unittest.main()
