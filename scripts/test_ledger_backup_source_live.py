from datetime import date, timedelta
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from capture_backup import BackupError
from capture_postgres import local_exec
from ledger_backup_bundle import write_bundle
from ledger_backup_source import require_logged_relations, require_quiet_database, require_single_volume_owner, source, spool_fingerprint
from verify_capture_backup import verify


ROOT = Path(__file__).resolve().parent.parent


@unittest.skipUnless(os.environ.get("RUN_LEDGER_SOURCE_DOCKER") == "1", "requires isolated Docker PostgreSQL")
class LedgerBackupSourceLiveTest(unittest.TestCase):
    def test_source_identifies_actual_isolated_database_and_rejects_capture(self):
        run_id = uuid.uuid4().hex
        label = "gateway.ledger_source_test=" + run_id
        expiry = "expire=" + (date.today() + timedelta(days=1)).isoformat()
        network = "gateway-ledger-source-net-" + run_id
        volume = "gateway-ledger-source-vol-" + run_id
        restored_volume = "gateway-ledger-restored-vol-" + run_id
        restored_db_volume = "gateway-ledger-restored-db-vol-" + run_id
        bundle_volume = "gateway-ledger-bundle-vol-" + run_id
        postgres = "gateway-ledger-source-db-" + run_id
        gateway = "gateway-ledger-source-gw-" + run_id
        capture_gateway = "gateway-ledger-source-capture-" + run_id
        upstream = "gateway-ledger-source-upstream-" + run_id
        restored = "gateway-ledger-source-restored-" + run_id
        restored_gateway = "gateway-ledger-source-restored-gw-" + run_id
        bundle_db = "gateway-ledger-bundle-db-" + run_id
        volume_probe = "gateway-ledger-volume-probe-" + run_id
        containers = [capture_gateway, volume_probe, gateway, restored_gateway, upstream, restored, bundle_db, postgres]
        stop_test = os.environ.get("LEDGER_TEST_STOP") == "1"
        pending_only = os.environ.get("LEDGER_TEST_PENDING_ONLY") == "1"
        stream_stop = os.environ.get("LEDGER_TEST_STREAM_STOP") == "1"
        stream_short_timeout = os.environ.get("LEDGER_TEST_STREAM_SHORT_TIMEOUT") == "1"
        v8_restore = os.environ.get("LEDGER_TEST_V8_RESTORE") == "1"
        watchdog_test = os.environ.get("LEDGER_TEST_WATCHDOG") == "1"
        if (pending_only or stream_stop or v8_restore or watchdog_test) and not stop_test:
            self.fail("pending-only and stream tests require stop test")
        locker = None
        try:
            subprocess.run(["docker", "network", "create", "--internal", "--label", expiry, "--label", label, "--label", "gateway.ledger_watchdog_test=" + run_id, network], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
            subprocess.run(["docker", "volume", "create", "--label", expiry, "--label", label, volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
            subprocess.run(["docker", "run", "-d", "--rm", "--label", expiry, "--label", label, "--pull=never", "--name", postgres, "--network", network, "-e", "POSTGRES_USER=gateway", "-e", "POSTGRES_PASSWORD=fixture-secret", "-e", "POSTGRES_DB=fixture_actual", "postgres:17-alpine", "-c", "max_prepared_transactions=10"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                with (ROOT / "queries/test_capture_database_identity_connection.sql").open("rb") as query:
                    ready = subprocess.run(["docker", "exec", "-i", postgres, "psql", "-X", "-h", "127.0.0.1", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=query, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5)
                if ready.returncode == 0 and ready.stdout.strip() == b"1":
                    break
                time.sleep(0.2)
            else:
                self.fail("isolated PostgreSQL did not become ready")
            destination = "/var/lib/agent-pool-gateway/ledger-spool"
            image = os.environ.get("LEDGER_TEST_GATEWAY_IMAGE", "")
            unconfirmed = bool(image and os.environ.get("LEDGER_TEST_CAPTURE_UNSET") == "1")
            send_request = bool(image and os.environ.get("LEDGER_TEST_REQUEST") == "1")
            if stop_test and not send_request:
                self.fail("stop test requires the real image and request test")
            if image:
                subprocess.run(["docker", "run", "--rm", "--label", expiry, "--label", label, "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target={destination}", "--entrypoint", "chown", "python:3.13-slim", "10001:999", destination], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
            if send_request:
                fixture = ROOT / "tests/fixtures/capture-backup/mock.py"
                subprocess.run(["docker", "run", "-d", "--rm", "--label", expiry, "--label", label, "--pull=never", "--name", upstream, "--network", network, "-e", "FIXTURE_STREAM_DELAY=" + ("12" if stream_short_timeout or stream_stop or watchdog_test else "3"), "--mount", f"type=bind,source={fixture},target=/mock.py,readonly", "--entrypoint", "python", "python:3.13-slim", "/mock.py"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
            for name, capture in ((gateway, "false"), (capture_gateway, "true")):
                arguments = ["docker", "run", "-d", "--label", expiry, "--label", label, "--label", "gateway.ledger_watchdog_test=" + run_id, "--pull=never", "--name", name, "--network", network, "--mount", f"type=volume,source={volume},target={destination}", "-e", f"DATABASE_URL=postgres://gateway:fixture-secret@{postgres}:5432/fixture_actual", "-e", "LEDGER_SPOOL_DIR=" + destination]
                if name != gateway or not stop_test:
                    arguments.insert(3, "--rm")
                if name != gateway or not unconfirmed:
                    arguments += ["-e", "CONTENT_CAPTURE_ENABLED=" + capture]
                if name == gateway and image:
                    upstream_url = f"http://{upstream}:8090" if send_request else "http://127.0.0.1:8090"
                    arguments += ["-e", "CREDENTIAL_HASH_KEY=fixture-hash-key-with-at-least-32-bytes", "-e", "CREDENTIAL_ENVIRONMENT=dev", "-e", "ADMIN_BOOTSTRAP_TOKEN=fixture-bootstrap-token-at-least-32-bytes", "-e", "ADMIN_BOOTSTRAP_TENANT_ID=00000000-0000-0000-0000-000000000001", "-e", "LISTEN_ADDRESS=0.0.0.0:8080", "-e", "OPENAI_BASE_URL=" + upstream_url, "-e", "OPENAI_API_KEY=fixture-provider-key", "-e", "ALLOWED_MODELS=company-default", image]
                else:
                    arguments += ["--entrypoint", "sleep", "python:3.13-slim", "300"]
                subprocess.run(arguments, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
            if image:
                deadline = time.monotonic() + 60
                while time.monotonic() < deadline:
                    ready = subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
                    if ready.returncode == 0:
                        break
                    time.sleep(0.2)
                else:
                    self.fail("isolated real Gateway did not become ready")
            if unconfirmed:
                with self.assertRaises(BackupError):
                    source(gateway, postgres)
            result = source(gateway, postgres, inspect_unconfirmed=unconfirmed)
            gateway_id = subprocess.run(["docker", "inspect", "--format", "{{.Id}}", gateway], capture_output=True, text=True, timeout=10, check=True).stdout.strip()
            postgres_id = subprocess.run(["docker", "inspect", "--format", "{{.Id}}", postgres], capture_output=True, text=True, timeout=10, check=True).stdout.strip()
            self.assertEqual(result["gateway_id"], gateway_id)
            self.assertEqual(result["postgres_id"], postgres_id)
            self.assertEqual(result["capture_setting"], "unconfirmed" if unconfirmed else "disabled")
            self.assertEqual(result["capture_disabled_setting_explicit"], not unconfirmed)
            self.assertNotIn("fixture-secret", json.dumps(result))
            with self.assertRaises(BackupError):
                source(capture_gateway, postgres)
            if watchdog_test:
                subprocess.run(["docker", "stop", "--time", "2", capture_gateway], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, check=True)
            if send_request:
                def api(path, token, payload, request_id=None):
                    command = ["docker", "exec", gateway, "curl", "-fsS", "--max-time", "10", "-H", "content-type: application/json", "-H", "authorization: Bearer " + token, "-X", "POST", "--data-binary", json.dumps(payload), "http://127.0.0.1:8080" + path]
                    if request_id:
                        command[-1:-1] = ["-H", "x-request-id: " + request_id]
                    response = subprocess.run(command, capture_output=True, text=True, timeout=15)
                    if response.returncode:
                        raise RuntimeError(f"isolated API failed: {path}, exit {response.returncode}")
                    return json.loads(response.stdout)

                boot = api("/admin/v1/bootstrap", "fixture-bootstrap-token-at-least-32-bytes", {"display_name": "test admin", "credential_name": "test admin"})
                session = api("/admin/v1/sessions", boot["login_token"], {})["access_token"]
                user = api("/admin/v1/users", session, {"display_name": "test user"})["resource"]["id"]
                key = api("/admin/v1/credentials", session, {"display_name": "test client", "user_id": user})["secret"]
                provider = api("/admin/v1/providers", session, {"provider_key": "openai", "display_name": "fixture", "provider_type": "openai", "base_url": f"http://{upstream}:8090", "credential_ref": "env://OPENAI_API_KEY", "supported_operations": ["openai_chat_completions"]})["resource"]["id"]
                model = api("/admin/v1/models", session, {"provider_id": provider, "public_model_id": "company-default", "provider_model_id": "company-default", "supported_operations": ["openai_chat_completions"], "supports_streaming": True})["resource"]["id"]
                api(f"/admin/v1/providers/{provider}/lifecycle", session, {"version": 1, "state": "active"})
                api(f"/admin/v1/models/{model}/lifecycle", session, {"version": 1, "state": "active"})
                reply = api("/v1/chat/completions", key, {"model": "company-default", "messages": [{"role": "user", "content": "backup-e2e-ledger-source"}]}, "ledger-source-live")
                self.assertEqual(reply["choices"][0]["message"]["content"], "fixture reply")
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    with (ROOT / "queries/test_capture_backup_ledger_counts.sql").open("rb") as query:
                        counts = subprocess.run(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1", "-v", "request=ledger-source-live"], stdin=query, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=True).stdout.strip()
                    values = counts.split(b"|")
                    if len(values) == 3 and values[0:2] == [b"1", b"1"] and values[2].isdigit() and int(values[2]) >= 1:
                        break
                    time.sleep(0.2)
                else:
                    self.fail("isolated request was not recorded in the ledger")
                self.assertEqual(source(gateway, postgres)["storage_identity"], result["storage_identity"])
                if stop_test:
                    if watchdog_test:
                        with tempfile.TemporaryDirectory(prefix="ledger-real-watchdog-") as watchdog_directory:
                            watchdog_log = Path(watchdog_directory) / "watchdog.log"
                            launcher_code = "import subprocess,sys; f=open(sys.argv[3],'wb',buffering=0); p=subprocess.Popen([sys.executable,sys.argv[1],sys.argv[2],sys.argv[4]],stdin=subprocess.PIPE,stdout=f,stderr=subprocess.DEVNULL,start_new_session=True); print(p.pid,flush=True); p.wait()"
                            parent = subprocess.Popen([sys.executable, "-c", launcher_code, str(ROOT / "scripts/ledger_watchdog_fixture.py"), gateway_id, str(watchdog_log), run_id], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
                            try:
                                self.assertTrue(select.select([parent.stdout], [], [], 10)[0])
                                self.assertTrue(parent.stdout.readline().strip().isdigit())
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if watchdog_log.exists() and b"STOPPED\n" in watchdog_log.read_bytes():
                                        break
                                    self.assertIsNone(parent.poll())
                                    time.sleep(0.1)
                                else:
                                    self.fail("isolated real Gateway was not stopped by watchdog")
                                parent.kill()
                                self.assertEqual(parent.wait(timeout=10), -9)
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if b"RESTARTED\n" in watchdog_log.read_bytes():
                                        break
                                    time.sleep(0.1)
                                else:
                                    self.fail("isolated real Gateway was not restarted by watchdog")
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                        break
                                    time.sleep(0.2)
                                else:
                                    self.fail("isolated real Gateway was not ready after watchdog restart")
                                with (ROOT / "queries/test_capture_backup_ledger_counts.sql").open("rb") as query:
                                    after = subprocess.run(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1", "-v", "request=ledger-source-live"], stdin=query, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=True).stdout.strip()
                                self.assertEqual(after, counts)
                            finally:
                                if parent.poll() is None:
                                    parent.kill()
                                    parent.wait(timeout=10)
                                parent.stdout.close()
                            gpg_home = Path(watchdog_directory) / "keys"
                            gpg_home.mkdir(mode=0o700)
                            gpg_env = dict(os.environ, GNUPGHOME=str(gpg_home))
                            subprocess.run(["gpg", "--batch", "--yes", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Fixture <fixture@example.invalid>", "rsa2048", "sign,encr", "0"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=gpg_env, timeout=60, check=True)
                            listing = subprocess.run(["gpg", "--batch", "--with-colons", "--list-secret-keys"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=gpg_env, timeout=20, check=True).stdout.decode()
                            fingerprint = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
                            backup = Path(watchdog_directory) / "backup"
                            guardian = subprocess.Popen([sys.executable, str(ROOT / "scripts/ledger_watchdog_fixture.py"), gateway_id, run_id, str(backup)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, bufsize=0)
                            original_home = os.environ.get("GNUPGHOME")
                            os.environ["GNUPGHOME"] = str(gpg_home)
                            try:
                                self.assertTrue(select.select([guardian.stdout], [], [], 30)[0])
                                self.assertRegex(guardian.stdout.readline().decode().strip(), r"^STOPPING [0-9]+$")
                                self.assertTrue(select.select([guardian.stdout], [], [], 30)[0])
                                self.assertEqual(guardian.stdout.readline().strip(), b"STOPPED")
                                require_single_volume_owner(volume, gateway_id)
                                subprocess.run(["docker", "run", "-d", "--rm", "--label", expiry, "--label", label, "--pull=never", "--name", volume_probe, "--network", "none", "--mount", f"type=volume,source={volume},target=/spool", "python:3.13-slim", "sleep", "300"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
                                try:
                                    with self.assertRaises(BackupError):
                                        require_single_volume_owner(volume, gateway_id)
                                finally:
                                    subprocess.run(["docker", "stop", "--time", "2", volume_probe], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, check=True)
                                deadline = time.monotonic() + 10
                                while time.monotonic() < deadline:
                                    if subprocess.run(["docker", "inspect", volume_probe], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode != 0:
                                        break
                                    time.sleep(0.1)
                                else:
                                    self.fail("isolated volume probe was not removed")
                                subprocess.run(["docker", "create", "--label", expiry, "--label", label, "--pull=never", "--name", volume_probe, "--network", "none", "--mount", f"type=volume,source={volume},target=/spool", "python:3.13-slim", "sleep", "300"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
                                try:
                                    with self.assertRaises(BackupError):
                                        require_single_volume_owner(volume, gateway_id)
                                finally:
                                    subprocess.run(["docker", "rm", "-v", volume_probe], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, check=True)
                                require_single_volume_owner(volume, gateway_id)
                                require_quiet_database(postgres_id, 5432, "gateway", "fixture_actual")
                                require_logged_relations(postgres_id, 5432, "gateway", "fixture_actual")
                                with (ROOT / "queries/test_ledger_create_unlogged.sql").open("rb") as query:
                                    subprocess.run(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=query, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=True)
                                try:
                                    with self.assertRaises(BackupError):
                                        require_logged_relations(postgres_id, 5432, "gateway", "fixture_actual")
                                finally:
                                    with (ROOT / "queries/test_ledger_drop_unlogged.sql").open("rb") as query:
                                        subprocess.run(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=query, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=True)
                                require_logged_relations(postgres_id, 5432, "gateway", "fixture_actual")
                                with (ROOT / "queries/test_ledger_prepare_transaction.sql").open("rb") as query:
                                    subprocess.run(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=query, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=True)
                                try:
                                    with self.assertRaises(BackupError):
                                        require_quiet_database(postgres_id, 5432, "gateway", "fixture_actual")
                                finally:
                                    with (ROOT / "queries/test_ledger_rollback_prepared.sql").open("rb") as query:
                                        subprocess.run(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=query, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=True)
                                require_quiet_database(postgres_id, 5432, "gateway", "fixture_actual")
                                writer = subprocess.Popen(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
                                try:
                                    writer.stdin.write((ROOT / "queries/test_capture_database_identity_connection.sql").read_bytes())
                                    writer.stdin.flush()
                                    self.assertTrue(select.select([writer.stdout], [], [], 10)[0])
                                    self.assertEqual(writer.stdout.readline().strip(), b"1")
                                    with self.assertRaises(BackupError):
                                        require_quiet_database(postgres_id, 5432, "gateway", "fixture_actual")
                                finally:
                                    writer.stdin.close()
                                    writer.wait(timeout=10)
                                    writer.stdout.close()
                                require_quiet_database(postgres_id, 5432, "gateway", "fixture_actual")
                                identity = {name: result[name] for name in ("gateway_id", "postgres_id", "database_system_id", "database_oid", "storage_identity")}
                                producers = {"gateway.dump.gpg": local_exec(postgres_id, 5432, "pg_dump") + ["-U", "gateway", "-d", "fixture_actual", "-Fc"], "ledger-spool.tar.gpg": ["docker", "run", "--rm", "--label", expiry, "--label", label, "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target=/spool,readonly", "--entrypoint", "tar", "python:3.13-slim", "-C", "/spool", "-cf", "-", "."]}
                                write_bundle(backup, producers, fingerprint, fingerprint, identity, guardian)
                                self.assertEqual(guardian.stdout.read().strip(), b"RESTARTED")
                                verify(backup, fingerprint, fingerprint, inspect_ledger_v8=True)
                                subprocess.run(["docker", "volume", "create", "--label", expiry, "--label", label, bundle_volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
                                subprocess.run(["docker", "run", "-d", "--rm", "--label", expiry, "--label", label, "--pull=never", "--name", bundle_db, "--network", "none", "-e", "POSTGRES_USER=gateway", "-e", "POSTGRES_PASSWORD=fixture-secret", "-e", "POSTGRES_DB=fixture_actual", "postgres:17-alpine"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if subprocess.run(["docker", "exec", bundle_db, "pg_isready", "-h", "127.0.0.1", "-U", "gateway"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                        break
                                    time.sleep(0.2)
                                else:
                                    self.fail("isolated bundle restore database did not start")
                                restored_result = subprocess.run([sys.executable, "scripts/restore_ledger_backup.py", "--backup", str(backup), "--signer", fingerprint, "--recipient", fingerprint, "--postgres", bundle_db, "--db-name", "fixture_actual", "--ledger", bundle_volume], cwd=ROOT, env=gpg_env, capture_output=True, timeout=180)
                                self.assertEqual(restored_result.returncode, 0, restored_result.stderr.decode(errors="replace"))
                                with (ROOT / "queries/test_capture_backup_ledger_counts.sql").open("rb") as query:
                                    restored_counts = subprocess.run(["docker", "exec", "-i", bundle_db, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1", "-v", "request=ledger-source-live"], stdin=query, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=True).stdout.strip()
                                self.assertEqual(restored_counts, counts)
                            finally:
                                if original_home is None:
                                    del os.environ["GNUPGHOME"]
                                else:
                                    os.environ["GNUPGHOME"] = original_home
                                if guardian.stdin and not guardian.stdin.closed:
                                    guardian.stdin.close()
                                if guardian.poll() is None:
                                    guardian.wait(timeout=30)
                                guardian.stdout.close()
                            deadline = time.monotonic() + 30
                            while time.monotonic() < deadline:
                                if subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                    break
                                time.sleep(0.2)
                            else:
                                self.fail("isolated Gateway did not recover after backup")
                            entry_backup = Path(watchdog_directory) / "entry-backup"
                            unavailable_key = subprocess.run([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", run_id, "--destination", str(entry_backup), "--lock", str(Path(watchdog_directory) / "entry.lock"), "--recipient", "0" * 40, "--signer", fingerprint], cwd=ROOT, env=gpg_env, capture_output=True, timeout=30)
                            self.assertNotEqual(unavailable_key.returncode, 0)
                            self.assertFalse(entry_backup.exists())
                            subprocess.run(["gpg", "--batch", "--yes", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "SignOnly <signonly@example.invalid>", "rsa2048", "sign", "0"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=gpg_env, timeout=60, check=True)
                            sign_only_listing = subprocess.run(["gpg", "--batch", "--with-colons", "--list-keys", "signonly@example.invalid"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=gpg_env, timeout=20, check=True).stdout.decode()
                            sign_only = next(line.split(":")[9] for line in sign_only_listing.splitlines() if line.startswith("fpr:"))
                            unusable_key = subprocess.run([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", run_id, "--destination", str(entry_backup), "--lock", str(Path(watchdog_directory) / "entry.lock"), "--recipient", sign_only, "--signer", fingerprint], cwd=ROOT, env=gpg_env, capture_output=True, timeout=30)
                            self.assertNotEqual(unusable_key.returncode, 0)
                            self.assertIn("無法加密".encode(), unusable_key.stderr)
                            self.assertFalse(entry_backup.exists())
                            existing = Path(watchdog_directory) / "existing-backup"
                            existing.mkdir(mode=0o700)
                            (existing / "keep").write_bytes(b"existing fixture")
                            occupied = subprocess.run([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", run_id, "--destination", str(existing), "--lock", str(Path(watchdog_directory) / "entry.lock"), "--recipient", fingerprint, "--signer", fingerprint], cwd=ROOT, env=gpg_env, capture_output=True, timeout=30)
                            self.assertNotEqual(occupied.returncode, 0)
                            self.assertEqual((existing / "keep").read_bytes(), b"existing fixture")
                            self.assertEqual(subprocess.run(["docker", "inspect", "--format", "{{.State.Running}}", gateway], capture_output=True, timeout=10, check=True).stdout.strip(), b"true")
                            wrong_entry = subprocess.run([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", uuid.uuid4().hex, "--destination", str(entry_backup), "--lock", str(Path(watchdog_directory) / "entry.lock"), "--recipient", fingerprint, "--signer", fingerprint], cwd=ROOT, env=gpg_env, capture_output=True, timeout=30)
                            self.assertNotEqual(wrong_entry.returncode, 0)
                            self.assertFalse(entry_backup.exists())
                            self.assertEqual(subprocess.run(["docker", "inspect", "--format", "{{.State.Running}}", gateway], capture_output=True, timeout=10, check=True).stdout.strip(), b"true")
                            entry = subprocess.run([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", run_id, "--destination", str(entry_backup), "--lock", str(Path(watchdog_directory) / "entry.lock"), "--recipient", fingerprint, "--signer", fingerprint], cwd=ROOT, env=gpg_env, capture_output=True, timeout=180)
                            self.assertEqual(entry.returncode, 0, entry.stderr.decode(errors="replace"))
                            previous_home = os.environ.get("GNUPGHOME")
                            os.environ["GNUPGHOME"] = str(gpg_home)
                            try:
                                verify(entry_backup, fingerprint, fingerprint, inspect_ledger_v8=True)
                            finally:
                                if previous_home is None:
                                    del os.environ["GNUPGHOME"]
                                else:
                                    os.environ["GNUPGHOME"] = previous_home
                            deadline = time.monotonic() + 30
                            while time.monotonic() < deadline:
                                if subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                    break
                                time.sleep(0.2)
                            else:
                                self.fail("isolated Gateway did not recover after backup entry")
                            crash_backup = Path(watchdog_directory) / "crash-backup"
                            crash_lock = Path(watchdog_directory) / "crash.lock"
                            producer_started = Path(watchdog_directory) / "producer-started"
                            wrapper = Path(watchdog_directory) / "docker"
                            real_docker = shutil.which("docker")
                            self.assertIsNotNone(real_docker)
                            wrapper.write_text("#!" + sys.executable + "\nimport os,subprocess,sys,time\nfrom pathlib import Path\ntarget=os.environ.get('LEDGER_TEST_HOLD_COMMAND','pg_dump')\nif target in sys.argv and (target!='tar' or any(x.startswith('gateway.ledger_watchdog_test=') for x in sys.argv)):\n parent=os.getppid(); Path(os.environ['LEDGER_TEST_STARTED']).write_text(str(os.getpid())); release=os.environ.get('LEDGER_TEST_RELEASE')\n while os.getppid()==parent and (not release or not Path(release).exists()): time.sleep(0.1)\n if os.getppid()!=parent: raise SystemExit(1)\n done=os.environ.get('LEDGER_TEST_TAR_DONE')\n if done:\n  result=subprocess.run([" + repr(real_docker) + "]+sys.argv[1:]); Path(done).touch(); finish=Path(os.environ['LEDGER_TEST_TAR_FINISH'])\n  while os.getppid()==parent and not finish.exists(): time.sleep(0.1)\n  raise SystemExit(result.returncode if os.getppid()==parent else 1)\nos.execv(" + repr(real_docker) + ",[" + repr(real_docker) + "]+sys.argv[1:])\n")
                            wrapper.chmod(0o700)
                            crash_env = dict(gpg_env, LEDGER_TEST_STARTED=str(producer_started), PATH=watchdog_directory + ":" + gpg_env["PATH"])
                            crashing = subprocess.Popen([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", run_id, "--destination", str(crash_backup), "--lock", str(crash_lock), "--recipient", fingerprint, "--signer", fingerprint], cwd=ROOT, env=crash_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                            try:
                                deadline = time.monotonic() + 40
                                while time.monotonic() < deadline:
                                    if producer_started.exists():
                                        break
                                    self.assertIsNone(crashing.poll())
                                    time.sleep(0.1)
                                else:
                                    self.fail("isolated backup did not reach the database producer")
                                state = subprocess.run(["docker", "inspect", "--format", "{{.State.Status}}", gateway], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=True).stdout.strip()
                                self.assertEqual(state, b"exited")
                                crashing.kill()
                                self.assertEqual(crashing.wait(timeout=10), -9)
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                        break
                                    time.sleep(0.2)
                                else:
                                    self.fail("isolated Gateway did not recover from killed backup")
                                self.assertFalse((crash_backup / ".complete").exists())
                                with crash_lock.open("rb") as lock_file:
                                    fcntl.flock(lock_file, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            finally:
                                if crashing.poll() is None:
                                    crashing.kill()
                                    crashing.wait(timeout=10)
                            swap_backup = Path(watchdog_directory) / "swap-backup"
                            moved_backup = Path(watchdog_directory) / "moved-backup"
                            swap_started = Path(watchdog_directory) / "swap-started"
                            swap_release = Path(watchdog_directory) / "swap-release"
                            swap_env = dict(crash_env, LEDGER_TEST_STARTED=str(swap_started), LEDGER_TEST_RELEASE=str(swap_release))
                            swapping = subprocess.Popen([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", run_id, "--destination", str(swap_backup), "--lock", str(Path(watchdog_directory) / "swap.lock"), "--recipient", fingerprint, "--signer", fingerprint], cwd=ROOT, env=swap_env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                            try:
                                deadline = time.monotonic() + 40
                                while time.monotonic() < deadline:
                                    if swap_started.exists():
                                        break
                                    self.assertIsNone(swapping.poll())
                                    time.sleep(0.1)
                                else:
                                    self.fail("isolated backup did not reach the directory swap point")
                                swap_backup.rename(moved_backup)
                                swap_backup.mkdir(mode=0o700)
                                (swap_backup / "keep").write_bytes(b"separate fixture")
                                swap_release.touch()
                                self.assertNotEqual(swapping.wait(timeout=90), 0)
                                self.assertIn("測試備份目的地已被替換".encode(), swapping.stderr.read())
                                self.assertEqual({item.name for item in swap_backup.iterdir()}, {"keep"})
                                self.assertEqual((swap_backup / "keep").read_bytes(), b"separate fixture")
                                self.assertFalse((moved_backup / ".complete").exists())
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                        break
                                    time.sleep(0.2)
                                else:
                                    self.fail("isolated Gateway did not recover after directory swap")
                            finally:
                                if swapping.poll() is None:
                                    swapping.kill()
                                    swapping.wait(timeout=10)
                                swapping.stderr.close()
                            guardian_backup = Path(watchdog_directory) / "guardian-crash-backup"
                            guardian_lock = Path(watchdog_directory) / "guardian-crash.lock"
                            guardian_started = Path(watchdog_directory) / "guardian-producer-started"
                            guardian_env = dict(crash_env, LEDGER_TEST_STARTED=str(guardian_started))
                            parent_run = subprocess.Popen([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", run_id, "--destination", str(guardian_backup), "--lock", str(guardian_lock), "--recipient", fingerprint, "--signer", fingerprint], cwd=ROOT, env=guardian_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                            try:
                                deadline = time.monotonic() + 40
                                while time.monotonic() < deadline:
                                    if guardian_started.exists():
                                        break
                                    self.assertIsNone(parent_run.poll())
                                    time.sleep(0.1)
                                else:
                                    self.fail("isolated guardian test did not reach the database producer")
                                children = (Path("/proc") / str(parent_run.pid) / "task" / str(parent_run.pid) / "children").read_text().split()
                                watchdogs = []
                                for child in children:
                                    try:
                                        args = (Path("/proc") / child / "cmdline").read_bytes().split(b"\0")
                                    except FileNotFoundError:
                                        continue
                                    if str(ROOT / "scripts/ledger_watchdog_fixture.py").encode() in args:
                                        watchdogs.append(int(child))
                                self.assertEqual(len(watchdogs), 1)
                                os.kill(watchdogs[0], signal.SIGKILL)
                                self.assertNotEqual(parent_run.wait(timeout=30), 0)
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                        break
                                    time.sleep(0.2)
                                else:
                                    self.fail("isolated Gateway did not recover from killed watchdog")
                                self.assertFalse((guardian_backup / ".complete").exists())
                                with guardian_lock.open("rb") as lock_file:
                                    fcntl.flock(lock_file, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            finally:
                                if parent_run.poll() is None:
                                    parent_run.kill()
                                    parent_run.wait(timeout=10)
                            client_code = "import json,sys,urllib.request; body=json.dumps({'model':'company-default','stream':True,'messages':[{'role':'user','content':'backup-e2e-midstop'}]}).encode(); req=urllib.request.Request(sys.argv[1],data=body,headers={'Content-Type':'application/json','Authorization':'Bearer '+sys.argv[2]}); response=urllib.request.urlopen(req,timeout=20);\nfor line in response: print(line.decode(),end='',flush=True)"
                            streaming = subprocess.Popen(["docker", "exec", upstream, "python", "-u", "-c", client_code, f"http://{gateway}:8080/v1/chat/completions", key], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
                            midstop = None
                            try:
                                self.assertTrue(select.select([streaming.stdout], [], [], 15)[0])
                                self.assertIn(b"fixture start", streaming.stdout.readline())
                                midstop_backup = Path(watchdog_directory) / "midstop-backup"
                                midstop = subprocess.Popen([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", run_id, "--destination", str(midstop_backup), "--lock", str(Path(watchdog_directory) / "midstop.lock"), "--recipient", fingerprint, "--signer", fingerprint], cwd=ROOT, env=gpg_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                                deadline = time.monotonic() + 15
                                stopped_guardian = None
                                while time.monotonic() < deadline:
                                    self.assertIsNone(midstop.poll())
                                    children = (Path("/proc") / str(midstop.pid) / "task" / str(midstop.pid) / "children").read_text().split()
                                    for child in children:
                                        try:
                                            args = (Path("/proc") / child / "cmdline").read_bytes().split(b"\0")
                                            stop_children = (Path("/proc") / child / "task" / child / "children").read_text().split()
                                        except FileNotFoundError:
                                            continue
                                        if str(ROOT / "scripts/ledger_watchdog_fixture.py").encode() in args and stop_children:
                                            stopped_guardian = int(child)
                                            break
                                    if stopped_guardian is not None:
                                        break
                                    time.sleep(0.05)
                                self.assertIsNotNone(stopped_guardian)
                                time.sleep(0.2)
                                self.assertIsNone(midstop.poll())
                                os.kill(stopped_guardian, signal.SIGKILL)
                                self.assertNotEqual(midstop.wait(timeout=40), 0)
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                        break
                                    time.sleep(0.2)
                                else:
                                    self.fail("isolated Gateway did not recover from watchdog death during stop")
                                self.assertFalse((midstop_backup / ".complete").exists())
                            finally:
                                if midstop is not None and midstop.poll() is None:
                                    midstop.kill()
                                    midstop.wait(timeout=10)
                                if streaming.poll() is None:
                                    streaming.terminate()
                                streaming.communicate(timeout=10)
                            time.sleep(8)
                            concurrent_backup = Path(watchdog_directory) / "concurrent-backup"
                            concurrent_started = Path(watchdog_directory) / "concurrent-started"
                            concurrent_release = Path(watchdog_directory) / "concurrent-release"
                            concurrent_env = dict(crash_env, LEDGER_TEST_STARTED=str(concurrent_started), LEDGER_TEST_RELEASE=str(concurrent_release))
                            concurrent = subprocess.Popen([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", run_id, "--destination", str(concurrent_backup), "--lock", str(Path(watchdog_directory) / "concurrent.lock"), "--recipient", fingerprint, "--signer", fingerprint], cwd=ROOT, env=concurrent_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                            extra_client = None
                            try:
                                deadline = time.monotonic() + 40
                                while time.monotonic() < deadline:
                                    if concurrent_started.exists():
                                        break
                                    self.assertIsNone(concurrent.poll())
                                    time.sleep(0.1)
                                else:
                                    self.fail("isolated backup did not reach the held database producer")
                                extra_client = subprocess.Popen(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
                                extra_client.stdin.write((ROOT / "queries/test_capture_database_identity_connection.sql").read_bytes())
                                extra_client.stdin.flush()
                                self.assertTrue(select.select([extra_client.stdout], [], [], 10)[0])
                                self.assertEqual(extra_client.stdout.readline().strip(), b"1")
                                concurrent_release.touch()
                                self.assertNotEqual(concurrent.wait(timeout=90), 0)
                                self.assertFalse((concurrent_backup / ".complete").exists())
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                        break
                                    time.sleep(0.2)
                                else:
                                    self.fail("isolated Gateway did not recover after concurrent connection")
                            finally:
                                if extra_client is not None:
                                    extra_client.stdin.close()
                                    extra_client.wait(timeout=10)
                                    extra_client.stdout.close()
                                if concurrent.poll() is None:
                                    concurrent.kill()
                                    concurrent.wait(timeout=10)
                            spool_backup = Path(watchdog_directory) / "spool-race-backup"
                            spool_started = Path(watchdog_directory) / "spool-race-started"
                            spool_release = Path(watchdog_directory) / "spool-race-release"
                            spool_env = dict(crash_env, LEDGER_TEST_STARTED=str(spool_started), LEDGER_TEST_RELEASE=str(spool_release), LEDGER_TEST_HOLD_COMMAND="tar")
                            spool_race = subprocess.Popen([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", run_id, "--destination", str(spool_backup), "--lock", str(Path(watchdog_directory) / "spool-race.lock"), "--recipient", fingerprint, "--signer", fingerprint], cwd=ROOT, env=spool_env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                            probe_created = False
                            try:
                                deadline = time.monotonic() + 40
                                while time.monotonic() < deadline:
                                    if spool_started.exists():
                                        break
                                    self.assertIsNone(spool_race.poll())
                                    time.sleep(0.1)
                                else:
                                    self.fail("isolated spool race did not reach the held producer")
                                before_spool = spool_fingerprint(volume, spool_race)
                                subprocess.run(["docker", "run", "--rm", "--label", expiry, "--label", label, "--pull=never", "--name", volume_probe, "--network", "none", "--user", "10001:999", "--mount", f"type=volume,source={volume},target=/spool", "--entrypoint", "python", "python:3.13-slim", "-c", "from pathlib import Path; Path('/spool/fixture-spool-race').write_bytes(b'fixture')"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
                                probe_created = True
                                deadline = time.monotonic() + 10
                                while time.monotonic() < deadline:
                                    if subprocess.run(["docker", "inspect", volume_probe], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode != 0:
                                        break
                                    time.sleep(0.1)
                                else:
                                    self.fail("isolated spool writer was not removed")
                                require_single_volume_owner(volume, gateway_id)
                                self.assertNotEqual(spool_fingerprint(volume, spool_race), before_spool)
                                require_quiet_database(postgres_id, 5432, "gateway", "fixture_actual")
                                spool_release.touch()
                                self.assertNotEqual(spool_race.wait(timeout=90), 0)
                                self.assertIn("入帳備份內容與來源不一致".encode(), spool_race.stderr.read())
                                self.assertFalse((spool_backup / ".complete").exists())
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                        break
                                    time.sleep(0.2)
                                else:
                                    self.fail("isolated Gateway did not recover after spool race")
                            finally:
                                if spool_race.poll() is None:
                                    spool_race.kill()
                                    spool_race.wait(timeout=10)
                                spool_race.stderr.close()
                                if probe_created:
                                    subprocess.run(["docker", "run", "--rm", "--label", expiry, "--label", label, "--pull=never", "--network", "none", "--user", "10001:999", "--mount", f"type=volume,source={volume},target=/spool", "--entrypoint", "python", "python:3.13-slim", "-c", "from pathlib import Path; Path('/spool/fixture-spool-race').unlink()"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
                            transient_backup = Path(watchdog_directory) / "transient-backup"
                            transient_started = Path(watchdog_directory) / "transient-started"
                            transient_release = Path(watchdog_directory) / "transient-release"
                            transient_done = Path(watchdog_directory) / "transient-done"
                            transient_finish = Path(watchdog_directory) / "transient-finish"
                            transient_env = dict(crash_env, LEDGER_TEST_STARTED=str(transient_started), LEDGER_TEST_RELEASE=str(transient_release), LEDGER_TEST_HOLD_COMMAND="tar", LEDGER_TEST_TAR_DONE=str(transient_done), LEDGER_TEST_TAR_FINISH=str(transient_finish))
                            spool_helper = ["docker", "run", "--rm", "--label", expiry, "--label", label, "--pull=never", "--network", "none", "--user", "10001:999", "--mount", f"type=volume,source={volume},target=/spool", "--entrypoint", "python", "python:3.13-slim", "-c"]
                            subprocess.run([*spool_helper, "from pathlib import Path; Path('/spool/fixture-revert-probe').write_bytes(b'a')"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
                            transient = subprocess.Popen([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", run_id, "--destination", str(transient_backup), "--lock", str(Path(watchdog_directory) / "transient.lock"), "--recipient", fingerprint, "--signer", fingerprint], cwd=ROOT, env=transient_env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                            altered_lock = False
                            original_times = None
                            try:
                                deadline = time.monotonic() + 40
                                while time.monotonic() < deadline:
                                    if transient_started.exists():
                                        break
                                    self.assertIsNone(transient.poll())
                                    time.sleep(0.1)
                                else:
                                    self.fail("isolated transient race did not reach the held producer")
                                baseline = spool_fingerprint(volume, transient)
                                original_times = subprocess.run([*spool_helper, "import json,os; s=os.stat('/spool/fixture-revert-probe'); print(json.dumps([s.st_atime_ns,s.st_mtime_ns,s.st_size]))"], capture_output=True, timeout=20, check=True).stdout
                                atime, mtime, size = json.loads(original_times)
                                self.assertEqual(size, 1)
                                subprocess.run([*spool_helper, "from pathlib import Path; Path('/spool/fixture-revert-probe').write_bytes(b'b')"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
                                altered_lock = True
                                transient_release.touch()
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if transient_done.exists():
                                        break
                                    self.assertIsNone(transient.poll())
                                    time.sleep(0.1)
                                else:
                                    self.fail("isolated transient archive did not finish")
                                subprocess.run([*spool_helper, "import os,sys; p='/spool/fixture-revert-probe'; open(p,'wb').write(b'a'); os.utime(p,ns=(int(sys.argv[1]),int(sys.argv[2])))", str(atime), str(mtime)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
                                altered_lock = False
                                self.assertEqual(spool_fingerprint(volume, transient), baseline)
                                transient_finish.touch()
                                self.assertNotEqual(transient.wait(timeout=90), 0)
                                self.assertIn("入帳備份內容與來源不一致".encode(), transient.stderr.read())
                                self.assertFalse((transient_backup / ".complete").exists())
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                        break
                                    time.sleep(0.2)
                                else:
                                    self.fail("isolated Gateway did not recover after transient spool race")
                            finally:
                                if transient.poll() is None:
                                    transient.kill()
                                    transient.wait(timeout=10)
                                transient.stderr.close()
                                if altered_lock and original_times is not None:
                                    atime, mtime, _ = json.loads(original_times)
                                    subprocess.run([*spool_helper, "import os,sys; p='/spool/fixture-revert-probe'; open(p,'wb').write(b'a'); os.utime(p,ns=(int(sys.argv[1]),int(sys.argv[2])))", str(atime), str(mtime)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
                                subprocess.run([*spool_helper, "from pathlib import Path; Path('/spool/fixture-revert-probe').unlink()"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
                            late_backup = Path(watchdog_directory) / "late-backup"
                            late_started = Path(watchdog_directory) / "late-started"
                            late_release = Path(watchdog_directory) / "late-release"
                            late_env = dict(crash_env, LEDGER_TEST_STARTED=str(late_started), LEDGER_TEST_RELEASE=str(late_release), LEDGER_TEST_HOLD_COMMAND="tar")
                            late = subprocess.Popen([sys.executable, "scripts/ledger_backup_isolated.py", "--gateway", gateway, "--postgres", postgres, "--nonce", run_id, "--destination", str(late_backup), "--lock", str(Path(watchdog_directory) / "late.lock"), "--recipient", fingerprint, "--signer", fingerprint], cwd=ROOT, env=late_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                            try:
                                deadline = time.monotonic() + 40
                                while time.monotonic() < deadline:
                                    if late_started.exists():
                                        break
                                    self.assertIsNone(late.poll())
                                    time.sleep(0.1)
                                else:
                                    self.fail("isolated backup did not reach the held spool producer")
                                with (ROOT / "queries/test_ledger_late_write.sql").open("rb") as query:
                                    subprocess.run(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=query, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=True)
                                late_release.touch()
                                self.assertNotEqual(late.wait(timeout=90), 0)
                                self.assertFalse((late_backup / ".complete").exists())
                                with (ROOT / "queries/test_ledger_late_present.sql").open("rb") as query:
                                    found = subprocess.run(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=query, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=True).stdout.strip()
                                self.assertEqual(found, b"t")
                                deadline = time.monotonic() + 30
                                while time.monotonic() < deadline:
                                    if subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                        break
                                    time.sleep(0.2)
                                else:
                                    self.fail("isolated Gateway did not restart after late writer")
                            finally:
                                if late.poll() is None:
                                    late.kill()
                                    late.wait(timeout=10)
                    if stream_stop:
                        client_code = "import json,sys,urllib.request,urllib.error; body=json.dumps({'model':'company-default','stream':True,'messages':[{'role':'user','content':'backup-e2e-stream'}]}).encode(); req=urllib.request.Request(sys.argv[1],data=body,headers={'Content-Type':'application/json','Authorization':'Bearer '+sys.argv[2]});\ntry: response=urllib.request.urlopen(req,timeout=20)\nexcept urllib.error.HTTPError as e: print(e.read().decode(),file=sys.stderr); raise\nfor line in response: print(line.decode(),end='',flush=True)"
                        client = subprocess.Popen(["docker", "exec", upstream, "python", "-u", "-c", client_code, f"http://{gateway}:8080/v1/chat/completions", key], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                        try:
                            self.assertTrue(select.select([client.stdout], [], [], 15)[0])
                            first = client.stdout.readline()
                            if not first:
                                self.fail("isolated stream client failed: " + client.stderr.read().decode(errors="replace"))
                            self.assertIn(b"fixture start", first)
                            stopping = subprocess.Popen(["docker", "stop", "--time", "10" if stream_short_timeout else "60", gateway], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                            try:
                                tail = client.communicate(timeout=25)[0]
                                if stream_short_timeout:
                                    self.assertNotIn(b"fixture end", tail)
                                    self.assertNotIn(b"[DONE]", tail)
                                else:
                                    self.assertEqual(client.returncode, 0)
                                    self.assertIn(b"fixture end", tail)
                                    self.assertIn(b"[DONE]", tail)
                                self.assertEqual(stopping.wait(timeout=20), 0)
                            finally:
                                if stopping.poll() is None:
                                    stopping.terminate()
                                    stopping.wait(timeout=10)
                            state = subprocess.run(["docker", "inspect", "--format", "{{.State.Status}} {{.State.ExitCode}}", gateway], capture_output=True, text=True, timeout=10, check=True).stdout.strip()
                            self.assertEqual(state, "exited 137" if stream_short_timeout else "exited 0")
                            subprocess.run(["docker", "start", gateway], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, check=True)
                            deadline = time.monotonic() + 30
                            while time.monotonic() < deadline:
                                if subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                    break
                                time.sleep(0.2)
                            else:
                                self.fail("isolated Gateway did not restart after stream")
                            if stream_short_timeout:
                                time.sleep(3)
                        finally:
                            if client.poll() is None:
                                client.terminate()
                            client.communicate(timeout=10)
                    before_pending = None
                    if pending_only:
                        before_pending = subprocess.run(["docker", "exec", postgres, "pg_dump", "-U", "gateway", "-d", "fixture_actual", "-Fc"], capture_output=True, timeout=60, check=True).stdout
                    locker = subprocess.Popen(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
                    locker.stdin.write((ROOT / "queries/test_ledger_hold_ingest.sql").read_bytes())
                    locker.stdin.flush()
                    self.assertTrue(select.select([locker.stdout], [], [], 10)[0])
                    self.assertEqual(locker.stdout.readline().strip(), b"locked")
                    reply = api("/v1/chat/completions", key, {"model": "company-default", "messages": [{"role": "user", "content": "backup-e2e-pending"}]}, "ledger-source-pending")
                    self.assertEqual(reply["choices"][0]["message"]["content"], "fixture reply")
                    inspect_spool = ["docker", "run", "--rm", "--label", expiry, "--label", label, "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target=/spool,readonly", "--entrypoint", "python", "python:3.13-slim", "-c", "import base64,json,pathlib; p=list(pathlib.Path('/spool').glob('*.ready')); print(json.dumps([[x.name,base64.b64encode(x.read_bytes()).decode()] for x in p]))"]
                    deadline = time.monotonic() + 30
                    pending = []
                    while time.monotonic() < deadline:
                        candidates = json.loads(subprocess.run(inspect_spool, capture_output=True, timeout=10, check=True).stdout)
                        pending = [(name, base64.b64decode(data)) for name, data in candidates if b"ledger-source-pending" in base64.b64decode(data)]
                        if pending:
                            break
                        time.sleep(0.2)
                    self.assertEqual(len(pending), 1)
                    locker.stdin.close()
                    self.assertEqual(locker.wait(timeout=10), 0)
                    locker.stdout.close()
                    locker = None
                    deadline = time.monotonic() + 30
                    while time.monotonic() < deadline:
                        with (ROOT / "queries/test_capture_backup_ledger_counts.sql").open("rb") as query:
                            pending_counts = subprocess.run(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1", "-v", "request=ledger-source-pending"], stdin=query, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=True).stdout.strip()
                        values = pending_counts.split(b"|")
                        if len(values) == 3 and values[0:2] == [b"1", b"1"] and values[2].isdigit() and int(values[2]) >= 1:
                            break
                        time.sleep(0.2)
                    else:
                        self.fail("isolated pending request was not ingested")
                    subprocess.run(["docker", "stop", "--time", "60", gateway], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=75, check=True)
                    state = subprocess.run(["docker", "inspect", "--format", "{{.State.Status}} {{.State.ExitCode}}", gateway], capture_output=True, text=True, timeout=10, check=True).stdout.strip()
                    self.assertEqual(state, "exited 0")
                    subprocess.run(["docker", "run", "--rm", "-i", "--label", expiry, "--label", label, "--pull=never", "--network", "none", "--user", "10001:999", "--mount", f"type=volume,source={volume},target=/spool", "--entrypoint", "python", "python:3.13-slim", "-c", "import pathlib,sys; p=pathlib.Path('/spool')/sys.argv[1]; p.write_bytes(sys.stdin.buffer.read())", pending[0][0]], input=pending[0][1], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
                    with tempfile.TemporaryDirectory(prefix="ledger-cold-test-") as temporary:
                        env = dict(os.environ, GNUPGHOME=temporary)
                        database = subprocess.run(["docker", "exec", postgres, "pg_dump", "-U", "gateway", "-d", "fixture_actual", "-Fc"], capture_output=True, timeout=60, check=True).stdout
                        if pending_only:
                            database = before_pending
                        spool = subprocess.run(["docker", "run", "--rm", "--label", expiry, "--label", label, "--pull=never", "--network", "none", "--mount", f"type=volume,source={volume},target=/spool,readonly", "--entrypoint", "tar", "python:3.13-slim", "-C", "/spool", "-cf", "-", "."], capture_output=True, timeout=30, check=True).stdout
                        decrypted = []
                        for data in (database, spool):
                            encrypted = subprocess.run(["gpg", "--batch", "--yes", "--pinentry-mode", "loopback", "--passphrase", "fixture-only", "--symmetric"], input=data, capture_output=True, env=env, timeout=30, check=True).stdout
                            self.assertNotEqual(encrypted, data)
                            clear = subprocess.run(["gpg", "--batch", "--yes", "--pinentry-mode", "loopback", "--passphrase", "fixture-only", "--decrypt"], input=encrypted, capture_output=True, env=env, timeout=30, check=True).stdout
                            self.assertEqual(clear, data)
                            damaged = bytearray(encrypted)
                            damaged[len(damaged) // 2] ^= 1
                            rejected = subprocess.run(["gpg", "--batch", "--yes", "--pinentry-mode", "loopback", "--passphrase", "fixture-only", "--decrypt"], input=damaged, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env, timeout=30)
                            self.assertNotEqual(rejected.returncode, 0)
                            decrypted.append(clear)
                        subprocess.run(["docker", "volume", "create", "--label", expiry, "--label", label, restored_volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
                        restore_spool = ["docker", "run", "--rm", "-i", "--label", expiry, "--label", label, "--pull=never", "--network", "none", "--mount", f"type=volume,source={restored_volume},target=/spool", "--entrypoint", "tar", "python:3.13-slim", "-C", "/spool"]
                        if not v8_restore:
                            subprocess.run([*restore_spool, "-xf", "-"], input=decrypted[1], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
                            subprocess.run([*restore_spool, "-df", "-"], input=decrypted[1], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
                        restored_args = ["docker", "run", "-d", "--rm", "--label", expiry, "--label", label, "--pull=never", "--name", restored, "--network", "none" if v8_restore else network, "-e", "POSTGRES_USER=gateway", "-e", "POSTGRES_PASSWORD=fixture-secret", "-e", "POSTGRES_DB=fixture_actual"]
                        if v8_restore:
                            subprocess.run(["docker", "volume", "create", "--label", expiry, "--label", label, restored_db_volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
                            restored_args += ["--mount", f"type=volume,source={restored_db_volume},target=/var/lib/postgresql/data"]
                        subprocess.run([*restored_args, "postgres:17-alpine"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
                        deadline = time.monotonic() + 30
                        while time.monotonic() < deadline:
                            if subprocess.run(["docker", "exec", restored, "pg_isready", "-h", "127.0.0.1", "-U", "gateway"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                break
                            time.sleep(0.2)
                        else:
                            self.fail("isolated restore database did not become ready")
                        if v8_restore:
                            def gpg(*args, data=None):
                                return subprocess.run(["gpg", "--batch", "--yes", *args], input=data, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env, timeout=60, check=True).stdout
                            gpg("--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Fixture <fixture@example.invalid>", "rsa2048", "sign,encr", "0")
                            listing = gpg("--with-colons", "--list-secret-keys").decode()
                            fingerprint = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
                            backup = Path(temporary) / "backup"
                            backup.mkdir(mode=0o700)
                            hashes, sizes = {}, {}
                            for name, data in (("gateway.dump.gpg", database), ("ledger-spool.tar.gpg", spool)):
                                encrypted = gpg("--trust-model", "always", "--recipient", fingerprint, "--encrypt", data=data)
                                (backup / name).write_bytes(encrypted)
                                (backup / name).chmod(0o600)
                                hashes[name] = hashlib.sha256(encrypted).hexdigest()
                                sizes[name] = len(encrypted)
                            manifest = {"version": 8, "profile": "ledger", "signer_fingerprint": fingerprint, "recipient_fingerprint": fingerprint, "postgres_id": result["postgres_id"], "database_system_id": result["database_system_id"], "database_oid": result["database_oid"], "storage_identity": result["storage_identity"], "sha256": hashes, "size": sizes}
                            raw = json.dumps(manifest).encode()
                            for name, data in (("manifest.json", raw), ("manifest.sig", gpg("--local-user", fingerprint, "--detach-sign", data=raw)), (".complete", b"ok\n")):
                                (backup / name).write_bytes(data)
                                (backup / name).chmod(0o600)
                            restored_result = subprocess.run([sys.executable, "scripts/restore_ledger_backup.py", "--backup", str(backup), "--signer", fingerprint, "--recipient", fingerprint, "--postgres", restored, "--db-name", "fixture_actual", "--ledger", restored_volume], cwd=ROOT, env=env, capture_output=True, timeout=180)
                            self.assertEqual(restored_result.returncode, 0, restored_result.stderr.decode(errors="replace"))
                            subprocess.run(["docker", "stop", "--time", "10", restored], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=True)
                            restored_args[restored_args.index("none")] = network
                            subprocess.run([*restored_args, "postgres:17-alpine"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
                            deadline = time.monotonic() + 30
                            while time.monotonic() < deadline:
                                if subprocess.run(["docker", "exec", restored, "pg_isready", "-h", "127.0.0.1", "-U", "gateway"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode == 0:
                                    break
                                time.sleep(0.2)
                            else:
                                self.fail("isolated restored database did not restart")
                        else:
                            subprocess.run(["docker", "exec", "-i", restored, "pg_restore", "-U", "gateway", "-d", "fixture_actual", "--exit-on-error"], input=decrypted[0], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60, check=True)
                        with (ROOT / "queries/test_capture_backup_ledger_counts.sql").open("rb") as query:
                            restored_counts = subprocess.run(["docker", "exec", "-i", restored, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1", "-v", "request=ledger-source-live"], stdin=query, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=True).stdout.strip()
                        self.assertEqual(restored_counts, counts)
                        if pending_only:
                            with (ROOT / "queries/test_capture_backup_ledger_counts.sql").open("rb") as query:
                                absent = subprocess.run(["docker", "exec", "-i", restored, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1", "-v", "request=ledger-source-pending"], stdin=query, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=True).stdout.strip()
                            self.assertEqual(absent, b"0|0|0")
                        subprocess.run(["docker", "run", "-d", "--rm", "--label", expiry, "--label", label, "--pull=never", "--name", restored_gateway, "--network", network, "--mount", f"type=volume,source={restored_volume},target={destination}", "-e", f"DATABASE_URL=postgres://gateway:fixture-secret@{restored}:5432/fixture_actual", "-e", "LEDGER_SPOOL_DIR=" + destination, "-e", "CONTENT_CAPTURE_ENABLED=false", "-e", "CREDENTIAL_HASH_KEY=fixture-hash-key-with-at-least-32-bytes", "-e", "CREDENTIAL_ENVIRONMENT=dev", "-e", "ADMIN_BOOTSTRAP_TOKEN=fixture-bootstrap-token-at-least-32-bytes", "-e", "ADMIN_BOOTSTRAP_TENANT_ID=00000000-0000-0000-0000-000000000001", "-e", "LISTEN_ADDRESS=0.0.0.0:8080", "-e", f"OPENAI_BASE_URL=http://{upstream}:8090", "-e", "OPENAI_API_KEY=fixture-provider-key", "-e", "ALLOWED_MODELS=company-default", image], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=True)
                        deadline = time.monotonic() + 30
                        while time.monotonic() < deadline:
                            remaining = subprocess.run(["docker", "run", "--rm", "--label", expiry, "--label", label, "--pull=never", "--network", "none", "--mount", f"type=volume,source={restored_volume},target=/spool,readonly", "--entrypoint", "test", "python:3.13-slim", "-e", "/spool/" + pending[0][0]], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
                            if remaining.returncode == 1:
                                break
                            self.assertEqual(remaining.returncode, 0)
                            time.sleep(0.2)
                        else:
                            self.fail("restored pending entry was not replayed")
                        with (ROOT / "queries/test_capture_backup_ledger_counts.sql").open("rb") as query:
                            replay_counts = subprocess.run(["docker", "exec", "-i", restored, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1", "-v", "request=ledger-source-pending"], stdin=query, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=True).stdout.strip()
                        self.assertEqual(replay_counts, pending_counts)
                    subprocess.run(["docker", "start", gateway], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, check=True)
                    deadline = time.monotonic() + 30
                    while time.monotonic() < deadline:
                        ready = subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
                        if ready.returncode == 0:
                            break
                        time.sleep(0.2)
                    else:
                        self.fail("isolated Gateway did not restart")
                    self.assertEqual(source(gateway, postgres)["storage_identity"], result["storage_identity"])
                    subprocess.run(["docker", "kill", "--signal", "KILL", gateway], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=True)
                    killed = subprocess.run(["docker", "inspect", "--format", "{{.State.Status}} {{.State.ExitCode}}", gateway], capture_output=True, text=True, timeout=10, check=True).stdout.strip()
                    self.assertEqual(killed, "exited 137")
                    subprocess.run(["docker", "start", gateway], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, check=True)
                    deadline = time.monotonic() + 30
                    while time.monotonic() < deadline:
                        ready = subprocess.run(["docker", "exec", gateway, "curl", "-fsS", "--max-time", "2", "http://127.0.0.1:8080/ready"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
                        if ready.returncode == 0:
                            break
                        time.sleep(0.2)
                    else:
                        self.fail("isolated Gateway did not restart after forced stop")
                    with (ROOT / "queries/test_capture_backup_ledger_counts.sql").open("rb") as query:
                        counts = subprocess.run(["docker", "exec", "-i", postgres, "psql", "-X", "-U", "gateway", "-d", "fixture_actual", "-Atq", "-v", "ON_ERROR_STOP=1", "-v", "request=ledger-source-live"], stdin=query, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=True).stdout.strip()
                    values = counts.split(b"|")
                    self.assertTrue(len(values) == 3 and values[0:2] == [b"1", b"1"] and values[2].isdigit() and int(values[2]) >= 1)
        finally:
            if locker is not None:
                locker.terminate()
                locker.wait(timeout=10)
                locker.stdin.close()
                locker.stdout.close()
            for name in containers:
                inspected = subprocess.run(["docker", "inspect", "--format", "{{index .Config.Labels \"gateway.ledger_source_test\"}}", name], capture_output=True, text=True, timeout=10)
                if inspected.returncode == 0 and inspected.stdout.strip() == run_id:
                    if name == gateway and stop_test:
                        subprocess.run(["docker", "rm", "-f", "-v", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
                    else:
                        subprocess.run(["docker", "stop", "--time", "2", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
            for kind, name in (("volume", bundle_volume), ("volume", restored_db_volume), ("volume", restored_volume), ("volume", volume), ("network", network)):
                inspected = subprocess.run(["docker", kind, "inspect", "--format", "{{index .Labels \"gateway.ledger_source_test\"}}", name], capture_output=True, text=True, timeout=10)
                if inspected.returncode == 0 and inspected.stdout.strip() == run_id:
                    subprocess.run(["docker", kind, "rm", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
            remaining = []
            for command in (["docker", "ps", "-aq", "--filter", "label=" + label], ["docker", "volume", "ls", "-q", "--filter", "label=" + label], ["docker", "network", "ls", "-q", "--filter", "label=" + label]):
                remaining.append(subprocess.run(command, capture_output=True, text=True, timeout=10, check=True).stdout.strip())
            if any(remaining):
                raise RuntimeError("isolated Docker resources remain")


if __name__ == "__main__":
    unittest.main()
