from datetime import date, timedelta
import io
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
import uuid


ROOT = Path(__file__).resolve().parent.parent


@unittest.skipUnless(os.environ.get("RUN_LEGACY_BACKUP_DOCKER") == "1", "需要隔離 Docker 驗收")
class LegacyBackupLiveTest(unittest.TestCase):
    def test_backup_and_restore_the_checked_database_and_volume(self):
        suffix = uuid.uuid4().hex[:12]
        project = "legacy-backup-test-" + suffix
        postgres = project + "-postgres-1"
        gateway = project + "-gateway-1"
        network = project + "-network"
        volume = project + "_ledger-spool"
        expiry = "expire=" + (date.today() + timedelta(days=1)).isoformat()
        label = "gateway.legacy-test=" + suffix

        def docker(*args, **kwargs):
            return subprocess.run(["docker", *args], check=True, capture_output=True, timeout=60, **kwargs)

        def sql(filename):
            return docker("exec", "-i", postgres, "psql", "-X", "-v", "ON_ERROR_STOP=1", "-U", "gateway", "-d", "gateway", "-Atq", input=(ROOT / "queries" / filename).read_bytes()).stdout.strip()

        try:
            docker("network", "create", "--label", expiry, "--label", label, network)
            docker("volume", "create", "--label", expiry, "--label", label, volume)
            docker("run", "-d", "--rm", "--pull=never", "--label", expiry, "--label", label, "--name", postgres, "--network", network, "-e", "POSTGRES_USER=gateway", "-e", "POSTGRES_PASSWORD=fixture-only", "-e", "POSTGRES_DB=gateway", "postgres:17-alpine")
            for _ in range(100):
                ready = subprocess.run(["docker", "exec", postgres, "pg_isready", "-h", "127.0.0.1", "-U", "gateway"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0
                if ready:
                    break
                time.sleep(0.2)
            self.assertTrue(ready)
            docker("run", "-d", "--pull=never", "--label", expiry, "--label", label, "--name", gateway, "--network", network, "-e", f"DATABASE_URL=postgres://gateway:fixture-only@{postgres}:5432/gateway", "-e", "LEDGER_SPOOL_DIR=/var/lib/agent-pool-gateway/ledger-spool", "-v", f"{volume}:/var/lib/agent-pool-gateway/ledger-spool", "python:3.13-slim", "sleep", "300")
            sql("test_legacy_backup_seed.sql")
            spool_name = uuid.uuid4().hex + ".ready"
            spool_content = b"synthetic-ledger-record"
            docker("run", "--rm", "-i", "--label", label, "--network", "none", "-v", f"{volume}:/spool", "alpine:3", "sh", "-c", 'cat > "/spool/$1"', "fixture", spool_name, input=spool_content)
            with tempfile.TemporaryDirectory(prefix="legacy-backup-live-") as temporary:
                root = Path(temporary)
                env = os.environ.copy()
                real_docker = shutil.which("docker")
                self.assertIsNotNone(real_docker)
                wrapper = root / "docker"
                wrapper.write_text("#!" + sys.executable + "\nimport os,sys\nargs=sys.argv[1:]\nif args and args[0]=='run': args[1:1]=['--label',os.environ['TEST_DOCKER_LABEL']]\nos.execv(" + repr(real_docker) + ",[" + repr(real_docker) + "]+args)\n")
                wrapper.chmod(0o755)
                env.update(COMPOSE_PROJECT=project, GATEWAY_DEPLOYMENT_LOCK=str(root / "deployment.lock"), TEST_DOCKER_LABEL=label, TMPDIR=str(root), PATH=f"{root}:{env['PATH']}")

                def backup(*args):
                    process = subprocess.Popen(["bash", "scripts/backup.sh", *args], cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
                    try:
                        stdout, stderr = process.communicate(timeout=120)
                    except BaseException:
                        try:
                            os.killpg(process.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        try:
                            process.communicate(timeout=5)
                        except subprocess.TimeoutExpired:
                            print("隔離測試程序未如期結束", file=sys.stderr)
                        raise
                    return subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)

                result = backup("create", str(root / "backups"))
                self.assertEqual(result.returncode, 0, result.stderr)
                destinations = list((root / "backups").iterdir())
                self.assertEqual(len(destinations), 1)
                self.assertEqual((destinations[0] / ".legacy-complete").read_bytes(), b"ok\n")
                docker("run", "--rm", "--label", label, "--network", "none", "-v", f"{volume}:/spool", "-v", f"{root}:/saved", "alpine:3", "sh", "-c", 'mv "/spool/$1" /saved/original', "fixture", spool_name)
                self.assertEqual((root / "original").read_bytes(), spool_content)
                sql("test_legacy_backup_change.sql")
                self.assertEqual(sql("test_legacy_backup_count.sql"), b"2")
                archive = destinations[0] / "ledger-spool.tgz"
                original = archive.read_bytes()
                mount = f"{destinations[0]}:/fixture"
                started = docker("inspect", "--format", "{{.State.StartedAt}}", gateway).stdout.strip()
                docker("run", "--rm", "-i", "--label", label, "--network", "none", "-v", mount, "alpine:3", "sh", "-c", "cat > /fixture/ledger-spool.tgz", input=original[:len(original) // 2])
                rejected = backup("restore", str(destinations[0]))
                self.assertNotEqual(rejected.returncode, 0)
                self.assertIn("備份未通過安全檢查", rejected.stderr)
                self.assertEqual(list(root.glob("gateway-legacy-restore.*")), [])
                self.assertEqual(sql("test_legacy_backup_count.sql"), b"2")
                self.assertEqual(docker("inspect", "--format", "{{.State.Status}}", gateway).stdout.strip(), b"running")
                self.assertEqual(docker("inspect", "--format", "{{.State.StartedAt}}", gateway).stdout.strip(), started)
                malicious = io.BytesIO()
                with tarfile.open(fileobj=malicious, mode="w:gz") as candidate:
                    entry = tarfile.TarInfo("../outside")
                    entry.size = 1
                    candidate.addfile(entry, io.BytesIO(b"x"))
                docker("run", "--rm", "-i", "--label", label, "--network", "none", "-v", mount, "alpine:3", "sh", "-c", "cat > /fixture/ledger-spool.tgz", input=malicious.getvalue())
                rejected_path = backup("restore", str(destinations[0]))
                self.assertNotEqual(rejected_path.returncode, 0)
                self.assertIn("備份未通過安全檢查", rejected_path.stderr)
                self.assertEqual(list(root.glob("gateway-legacy-restore.*")), [])
                self.assertEqual(sql("test_legacy_backup_count.sql"), b"2")
                self.assertEqual(docker("inspect", "--format", "{{.State.StartedAt}}", gateway).stdout.strip(), started)
                self.assertFalse((root / "outside").exists())
                docker("run", "--rm", "-i", "--label", label, "--network", "none", "-v", mount, "alpine:3", "sh", "-c", "cat > /fixture/ledger-spool.tgz", input=original)
                restored = backup("restore", str(destinations[0]))
                self.assertEqual(restored.returncode, 0, restored.stderr)
                self.assertEqual(list(root.glob("gateway-legacy-restore.*")), [])
                self.assertEqual(sql("test_legacy_backup_count.sql"), b"1")
                status = docker("inspect", "--format", "{{.State.Status}}", gateway).stdout.strip()
                self.assertEqual(status, b"running")
                self.assertNotEqual(docker("inspect", "--format", "{{.State.StartedAt}}", gateway).stdout.strip(), started)
                restored_spool = docker("run", "--rm", "--label", label, "--network", "none", "-v", f"{volume}:/spool:ro", "alpine:3", "cat", f"/spool/{spool_name}").stdout
                self.assertEqual(restored_spool, spool_content)
                ownership = docker("run", "--rm", "--label", label, "--network", "none", "-v", f"{volume}:/spool:ro", "alpine:3", "stat", "-c", "%u:%g", f"/spool/{spool_name}").stdout.strip()
                self.assertEqual(ownership, b"10001:999")
        finally:
            active_error = sys.exc_info()[0] is not None
            errors = []
            for listing, removal in ((["ps", "-aq"], ["rm", "-f", "-v"]), (["volume", "ls", "-q"], ["volume", "rm"]), (["network", "ls", "-q"], ["network", "rm"])):
                try:
                    ids = docker(*listing, "--filter", "label=" + label).stdout.decode().split()
                    if ids:
                        docker(*removal, *ids)
                    if docker(*listing, "--filter", "label=" + label).stdout.strip():
                        errors.append("測試資源仍存在")
                except (OSError, subprocess.SubprocessError) as error:
                    errors.append(type(error).__name__)
            if errors:
                if active_error:
                    print("隔離測試清理失敗：" + ", ".join(errors), file=sys.stderr)
                else:
                    self.fail("隔離測試清理失敗：" + ", ".join(errors))


if __name__ == "__main__":
    unittest.main()
