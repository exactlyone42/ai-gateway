from datetime import date, timedelta
import fcntl
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


@unittest.skipUnless(os.environ.get("RUN_LEDGER_SYSTEMD_DOCKER") == "1", "requires isolated Docker and user systemd")
class IndependentWatchdogTest(unittest.TestCase):
    def test_user_service_recovers_after_parent_and_guardian_die(self):
        nonce = uuid.uuid4().hex
        label = "gateway.ledger_watchdog_test=" + nonce
        expiry = "expire=" + (date.today() + timedelta(days=1)).isoformat()
        name = "gateway-systemd-fixture-" + nonce
        unit = "gateway-watchdog-fixture-" + nonce
        runtime = "/run/user/" + str(os.getuid())
        env = dict(os.environ, XDG_RUNTIME_DIR=runtime, DBUS_SESSION_BUS_ADDRESS="unix:path=" + runtime + "/bus")
        if subprocess.run(["systemctl", "--user", "show-environment"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10).returncode:
            self.skipTest("user systemd manager unavailable")
        parent = None
        try:
            fixture = "import signal,sys,time; signal.signal(signal.SIGTERM,lambda *_:sys.exit(0)); print('READY',flush=True); time.sleep(300)"
            container = subprocess.run(["docker", "run", "-d", "--label", expiry, "--label", label, "--pull=never", "--name", name, "--network", "none", "python:3.13-slim", "python", "-u", "-c", fixture], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=30, check=True).stdout.decode().strip()
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if b"READY" in subprocess.run(["docker", "logs", container], capture_output=True, timeout=5, check=True).stdout:
                    break
                time.sleep(0.1)
            else:
                self.fail("fixture did not start")
            with tempfile.TemporaryDirectory(prefix="gateway-systemd-test-") as directory:
                root = Path(directory)
                lock_file = root / "operation.lock"
                watchdog_log = root / "watchdog.log"
                armed = root / "armed"
                recovered = root / "recovered"
                launch = "import fcntl,subprocess,sys,time; f=open(sys.argv[3],'wb',buffering=0); lock=open(sys.argv[5],'a'); fcntl.flock(lock,fcntl.LOCK_EX); p=subprocess.Popen([sys.executable,sys.argv[1],sys.argv[2],sys.argv[4]],stdin=subprocess.PIPE,stdout=f,stderr=subprocess.DEVNULL,pass_fds=(lock.fileno(),),start_new_session=True); print(p.pid,flush=True); p.wait(); time.sleep(30)"
                parent = subprocess.Popen([sys.executable, "-c", launch, str(ROOT / "scripts/ledger_watchdog_fixture.py"), container, str(watchdog_log), nonce, str(lock_file)], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
                guardian_pid = int(parent.stdout.readline().strip())
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    if watchdog_log.exists() and b"STOPPED\n" in watchdog_log.read_bytes():
                        break
                    self.assertIsNone(parent.poll())
                    time.sleep(0.1)
                else:
                    self.fail("fixture watchdog did not stop container")
                recovery = "import fcntl,json,subprocess,sys; from pathlib import Path; p=Path(sys.argv[3]); p.write_text('armed'); lock=open(sys.argv[2],'a'); fcntl.flock(lock,fcntl.LOCK_EX); info=json.loads(subprocess.check_output(['docker','inspect',sys.argv[1]]))[0]; assert info['Id']==sys.argv[1] and info['Config']['Labels'].get('gateway.ledger_watchdog_test')==sys.argv[4] and info['HostConfig']['NetworkMode']=='none'; subprocess.run(['docker','start',sys.argv[1]],check=True,stdout=subprocess.DEVNULL) if info['State']['Status']=='exited' else None; info=json.loads(subprocess.check_output(['docker','inspect',sys.argv[1]]))[0]; assert info['State']['Running']; Path(sys.argv[5]).write_text('recovered')"
                subprocess.run(["systemd-run", "--user", "--collect", "--unit", unit, "--property=RuntimeMaxSec=45s", sys.executable, "-c", recovery, container, str(lock_file), str(armed), nonce, str(recovered)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, check=True)
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    if armed.exists():
                        break
                    time.sleep(0.1)
                else:
                    self.fail("independent fixture service did not start")
                self.assertFalse(recovered.exists())
                os.kill(guardian_pid, signal.SIGKILL)
                self.assertIsNone(parent.poll())
                parent.kill()
                self.assertEqual(parent.wait(timeout=10), -9)
                parent.stdout.close()
                parent = None
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    if recovered.exists():
                        break
                    time.sleep(0.1)
                else:
                    self.fail("independent fixture service did not restore container")
                details = json.loads(subprocess.run(["docker", "inspect", container], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=True).stdout)[0]
                self.assertTrue(details["State"]["Running"])
                with lock_file.open("rb") as check:
                    fcntl.flock(check, fcntl.LOCK_EX | fcntl.LOCK_NB)
        finally:
            if parent is not None:
                parent.kill()
                parent.wait(timeout=10)
                parent.stdout.close()
            subprocess.run(["systemctl", "--user", "stop", unit + ".service"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
            owner = subprocess.run(["docker", "inspect", "--format", "{{index .Config.Labels \"gateway.ledger_watchdog_test\"}}", name], capture_output=True, text=True, timeout=10)
            if owner.returncode == 0 and owner.stdout.strip() == nonce:
                subprocess.run(["docker", "rm", "-f", "-v", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
            remaining = subprocess.run(["docker", "ps", "-aq", "--filter", "label=" + label], capture_output=True, timeout=10, check=True).stdout.strip()
            self.assertEqual(remaining, b"")


if __name__ == "__main__":
    unittest.main()
