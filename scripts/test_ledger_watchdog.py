from datetime import date, timedelta
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
import uuid


ROOT = Path(__file__).resolve().parent.parent


@unittest.skipUnless(os.environ.get("RUN_LEDGER_WATCHDOG_DOCKER") == "1", "requires isolated Docker")
class LedgerWatchdogTest(unittest.TestCase):
    def test_parent_kill_restarts_only_fixture_container(self):
        nonce = uuid.uuid4().hex
        label = "gateway.ledger_watchdog_test=" + nonce
        expiry = "expire=" + (date.today() + timedelta(days=1)).isoformat()
        name = "ledger-watchdog-fixture-" + nonce
        during_stop = os.environ.get("LEDGER_WATCHDOG_DURING_STOP") == "1"
        kill_guardian = os.environ.get("LEDGER_WATCHDOG_KILL_GUARDIAN") == "1"
        if kill_guardian and not during_stop:
            self.fail("guardian kill test requires stop in progress")
        parent = None
        try:
            fixture = "import signal,sys,time\ndef stop(*_):\n print('DRAINING',flush=True); time.sleep(2); sys.exit(0)\nsignal.signal(signal.SIGTERM,stop); print('READY',flush=True); time.sleep(300)"
            container = subprocess.run(["docker", "run", "-d", "--label", expiry, "--label", label, "--pull=never", "--name", name, "--network", "none", "python:3.13-slim", "python", "-u", "-c", fixture], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=30, check=True).stdout.decode().strip()
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if b"READY" in subprocess.run(["docker", "logs", container], capture_output=True, timeout=5, check=True).stdout:
                    break
                time.sleep(0.1)
            else:
                self.fail("fixture container did not start")
            wrong = subprocess.run([sys.executable, str(ROOT / "scripts/ledger_watchdog_fixture.py"), container, uuid.uuid4().hex], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
            self.assertNotEqual(wrong.returncode, 0)
            running = subprocess.run(["docker", "inspect", "--format", "{{.State.Running}}", container], capture_output=True, timeout=10, check=True).stdout.strip()
            self.assertEqual(running, b"true")
            with tempfile.TemporaryDirectory(prefix="ledger-watchdog-") as directory:
                log = Path(directory) / "watchdog.log"
                code = "import subprocess,sys; f=open(sys.argv[3],'wb',buffering=0); p=subprocess.Popen([sys.executable,sys.argv[1],sys.argv[2],sys.argv[4]],stdin=subprocess.PIPE,stdout=f,stderr=subprocess.DEVNULL,start_new_session=True); print(p.pid,flush=True); p.wait()"
                parent = subprocess.Popen([sys.executable, "-c", code, str(ROOT / "scripts/ledger_watchdog_fixture.py"), container, str(log), nonce], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
                pid = int(parent.stdout.readline().strip())
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    container_log = subprocess.run(["docker", "logs", container], capture_output=True, timeout=5, check=True).stdout
                    if during_stop and b"DRAINING\n" in container_log:
                        state = subprocess.run(["docker", "inspect", "--format", "{{.State.Running}}", container], capture_output=True, timeout=5, check=True).stdout.strip()
                        self.assertEqual(state, b"true")
                        break
                    if not during_stop and log.exists() and b"STOPPED\n" in log.read_bytes():
                        break
                    self.assertIsNone(parent.poll())
                    time.sleep(0.1)
                else:
                    self.fail("fixture watchdog did not stop the container")
                if kill_guardian:
                    os.kill(pid, signal.SIGKILL)
                    self.assertEqual(parent.wait(timeout=10), 0)
                else:
                    parent.kill()
                    self.assertEqual(parent.wait(timeout=10), -9)
                parent.stdout.close()
                parent = None
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    if kill_guardian:
                        current = subprocess.run(["docker", "inspect", "--format", "{{.State.Status}}", container], capture_output=True, timeout=5, check=True).stdout.strip()
                        if current == b"exited":
                            break
                    elif b"RESTARTED\n" in log.read_bytes():
                        break
                    time.sleep(0.1)
                else:
                    self.fail(f"fixture watchdog {pid} did not restart the container")
                info = json.loads(subprocess.run(["docker", "inspect", container], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=True).stdout)[0]
                self.assertEqual(info["State"]["Running"], not kill_guardian)
                if kill_guardian:
                    self.assertNotIn(b"RESTARTED\n", log.read_bytes())
                self.assertEqual(info["Config"]["Labels"]["gateway.ledger_watchdog_test"], nonce)
        finally:
            if parent is not None:
                parent.kill()
                parent.wait(timeout=10)
                parent.stdout.close()
            inspected = subprocess.run(["docker", "inspect", "--format", "{{index .Config.Labels \"gateway.ledger_watchdog_test\"}}", name], capture_output=True, text=True, timeout=10)
            if inspected.returncode == 0 and inspected.stdout.strip() == nonce:
                subprocess.run(["docker", "rm", "-f", "-v", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
            remaining = subprocess.run(["docker", "ps", "-aq", "--filter", "label=" + label], capture_output=True, timeout=10, check=True).stdout.strip()
            self.assertEqual(remaining, b"")


if __name__ == "__main__":
    unittest.main()
