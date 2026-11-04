import fcntl
import io
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest


SCRIPT = Path(__file__).with_name("backup.sh")
GATEWAY_ID = "a" * 64
POSTGRES_ID = "b" * 64
STARTED = "2026-11-03T00:00:00Z"
IDENTITY = f"{GATEWAY_ID} {POSTGRES_ID} {STARTED} {STARTED} 5432"


class BackupCaptureGuardTest(unittest.TestCase):
    def test_backup_operations_fail_before_touching_sources_when_deployment_lock_is_held(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            state = root / ".local" / "state"
            state.mkdir(parents=True)
            lock_path = state / "gateway-deployment.lock"
            log = root / "commands"
            for name in ("docker", "python3"):
                tool = root / name
                tool.write_text("#!/bin/sh\nprintf '%s\\n' invoked >> \"$TEST_COMMANDS\"\nexit 1\n")
                tool.chmod(0o755)
            env = os.environ.copy()
            env.update(GATEWAY_DEPLOYMENT_LOCK=str(lock_path), PATH=f"{root}:{env['PATH']}", TEST_COMMANDS=str(log))
            with lock_path.open("a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                for operation in (["create", str(root / "backup")], ["restore", str(root / "backup")], ["restore-capture", "--help"]):
                    with self.subTest(operation=operation[0]):
                        result = subprocess.run(["bash", str(SCRIPT), *operation], env=env, capture_output=True, text=True)
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn("部署、備份或還原正在執行", result.stderr)
                        self.assertFalse(log.exists())
                        self.assertFalse((root / "backup").exists())
                deploy = Path(__file__).with_name("deploy_gateway.py")
                result = subprocess.run([sys.executable, str(deploy), "--image", "fixture", "--revision", "a" * 12, "--source-container", "fixture", "--port", "9876", "--evidence", str(root / "evidence")], env=env, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("部署、備份或還原正在執行", result.stderr)
                self.assertFalse(log.exists())
                self.assertFalse((root / "evidence").exists())

    def test_capture_restore_entry_passes_explicit_isolated_targets(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "commands"
            python = root / "python3"
            python.write_text("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$TEST_COMMANDS\"\nexit 7\n")
            python.chmod(0o755)
            env = os.environ.copy()
            env.update(GATEWAY_DEPLOYMENT_LOCK=str(root / "deployment.lock"), PATH=f"{root}:{env['PATH']}", TEST_COMMANDS=str(log))
            args = ["--backup", "backup", "--signer", "A" * 40, "--postgres", "isolated-db", "--spool", "empty-spool", "--keys", "empty-keys", "--ledger", "empty-ledger", "--anchors", "empty-anchors"]
            result = subprocess.run(["bash", str(SCRIPT), "restore-capture", *args], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 7)
            commands = log.read_text().splitlines()
            self.assertTrue(commands[0].endswith("/restore_capture_backup.py"))
            self.assertEqual(commands[1:], args)

    def test_backup_holds_deployment_lock_until_operation_exits(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            marker = root / "started"
            lock_path = root / "deployment.lock"
            docker = root / "docker"
            docker.write_text("#!/bin/sh\nprintf ready > \"$TEST_MARKER\"\nIFS= read -r release\nexit 1\n")
            docker.chmod(0o755)
            env = os.environ.copy()
            env.update(GATEWAY_DEPLOYMENT_LOCK=str(lock_path), PATH=f"{root}:{env['PATH']}", TEST_MARKER=str(marker))
            process = subprocess.Popen(["bash", str(SCRIPT), "create", str(root / "backup")], env=env, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                deadline = time.monotonic() + 5
                while not marker.exists() and process.poll() is None and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(marker.exists())
                with lock_path.open("a") as lock:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                deploy = Path(__file__).with_name("deploy_gateway.py")
                result = subprocess.run([sys.executable, str(deploy), "--image", "fixture", "--revision", "a" * 12, "--source-container", "fixture", "--port", "9876", "--evidence", str(root / "evidence")], env=env, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("部署、備份或還原正在執行", result.stderr)
                self.assertFalse((root / "evidence").exists())
                process.stdin.write(b"release\n")
                process.stdin.flush()
                self.assertNotEqual(process.wait(timeout=5), 0)
                with lock_path.open("a") as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)
                process.stdin.close()

    def test_backup_and_deploy_reject_relative_lock_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            env = os.environ.copy()
            env["GATEWAY_DEPLOYMENT_LOCK"] = "relative.lock"
            commands = [
                ["bash", str(SCRIPT.resolve()), "create", str(root / "backup")],
                [sys.executable, str(Path(__file__).with_name("deploy_gateway.py").resolve()), "--image", "fixture", "--revision", "a" * 12, "--source-container", "fixture", "--port", "9876", "--evidence", str(root / "evidence")],
            ]
            for command in commands:
                result = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("部署鎖路徑必須是絕對路徑", result.stderr)
            self.assertFalse((root / "relative.lock").exists())
            self.assertFalse((root / "backup").exists())
            self.assertFalse((root / "evidence").exists())

    def run_operation(self, inventory, existing, inspect_status="0", operation="restore", legacy_mount="running apg_ledger-spool", source_status="0", postgres_started=STARTED, spool_check_status="0", restore_status="0", tar_status="0", complete=True, extract_status="0"):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        backup = root / "backup"
        backup.mkdir()
        (backup / "gateway.dump").write_bytes(b"test")
        with tarfile.open(backup / "ledger-spool.tgz", "w:gz") as archive:
            item = tarfile.TarInfo("c" * 32 + ".ready")
            item.size = 4
            archive.addfile(item, io.BytesIO(b"test"))
        if complete:
            (backup / ".legacy-complete").write_bytes(b"ok\n")
        log = root / "docker.log"
        docker = root / "docker"
        docker.write_text(
            "#!/bin/sh\n"
            "printf '%s\\n' \"$*\" >> \"$TEST_DOCKER_LOG\"\n"
            "case \"$*\" in\n"
            f"  *'{{{{.Id}}}}'*) printf '%s %s %s\\n' '{POSTGRES_ID}' running \"$TEST_POSTGRES_STARTED\";;\n"
            "  *'--format'*) printf '%s\\n' \"$TEST_LEGACY_MOUNT\";;\n"
            "  *'pg_restore --list'*) printf '%s\\n' \"$TEST_INVENTORY\"; exit \"$TEST_INSPECT_STATUS\";;\n"
            "  *'pg_restore -U gateway'*) exit \"$TEST_RESTORE_STATUS\";;\n"
            "  *'tar -tzf'*) exit \"$TEST_TAR_STATUS\";;\n"
            "  *'tar -xzf'*) exit \"$TEST_EXTRACT_STATUS\";;\n"
            "  *'find /spool'*) exit \"$TEST_SPOOL_CHECK_STATUS\";;\n"
            "  *'psql -U gateway'*) printf '%s\\n' \"$TEST_EXISTING\";;\n"
            "esac\n"
            "exit 0\n"
        )
        docker.chmod(0o755)
        python = root / "python3"
        python.write_text(f"#!/bin/sh\ncase \"$1\" in */stage_legacy_backup.py) exec {shlex.quote(sys.executable)} \"$@\";; esac\n[ \"$TEST_SOURCE_STATUS\" = 0 ] && printf '%s\\n' '{IDENTITY}'\nexit \"$TEST_SOURCE_STATUS\"\n")
        python.chmod(0o755)
        env = os.environ.copy()
        for name in ("CAPTURE_BACKUP_GATEWAY", "CAPTURE_BACKUP_RECIPIENT", "CAPTURE_BACKUP_SIGNER"):
            env.pop(name, None)
        env.update(
            GATEWAY_DEPLOYMENT_LOCK=str(root / "deployment.lock"),
            PATH=f"{root}:{env['PATH']}",
            TEST_DOCKER_LOG=str(log),
            TEST_INVENTORY=inventory,
            TEST_EXISTING=existing,
            TEST_INSPECT_STATUS=inspect_status,
            TEST_LEGACY_MOUNT=legacy_mount,
            TEST_SOURCE_STATUS=source_status,
            TEST_POSTGRES_STARTED=postgres_started,
            TEST_SPOOL_CHECK_STATUS=spool_check_status,
            TEST_RESTORE_STATUS=restore_status,
            TEST_TAR_STATUS=tar_status,
            TEST_EXTRACT_STATUS=extract_status,
            TMPDIR=str(root),
        )
        result = subprocess.run(["bash", str(SCRIPT), operation, str(backup)], env=env, capture_output=True, text=True)
        return result, log.read_text() if log.exists() else ""

    def test_capture_in_backup_is_rejected_before_stopping_gateway(self):
        for table in ("capture_records", "capture_blocks", "capture_master_key_usage", "capture_erasure_marks", "capture_erasure_floor", "capture_future_fixture"):
            with self.subTest(table=table):
                result, log = self.run_operation(f"123; 1259 456 TABLE public {table} gateway", "f")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("safe capture restore is not implemented", result.stderr)
                self.assertNotIn("stop ", log)

    def test_capture_in_live_database_is_rejected_before_stopping_gateway(self):
        result, log = self.run_operation("123; 1259 456 TABLE public users gateway", "t")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("safe capture backup and restore are not implemented", result.stderr)
        self.assertNotIn("stop ", log)

    def test_large_inventory_with_capture_table_is_rejected(self):
        inventory = "123; 1259 456 TABLE public capture_blocks gateway\n" + "filler\n" * 12000
        result, log = self.run_operation(inventory, "f")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("safe capture restore is not implemented", result.stderr)
        self.assertNotIn("stop ", log)

    def test_unreadable_backup_is_rejected_before_stopping_gateway(self):
        result, log = self.run_operation("", "f", "1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot inspect backup", result.stderr)
        self.assertNotIn("stop ", log)

    def test_corrupt_spool_backup_is_rejected_before_stopping_gateway(self):
        result, log = self.run_operation("123; 1259 456 TABLE public users gateway", "f", tar_status="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("入帳備份無法讀取", result.stderr)
        self.assertNotIn("stop ", log)
        self.assertNotIn("pg_restore -U gateway -d gateway --clean", log)

    def test_incomplete_legacy_backup_is_rejected_before_touching_sources(self):
        result, log = self.run_operation("123; 1259 456 TABLE public users gateway", "f", complete=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("備份缺少完成紀錄", result.stderr)
        self.assertEqual(log, "")

    def test_legacy_backup_continues_through_existing_restore_path(self):
        result, log = self.run_operation("123; 1259 456 TABLE public users gateway", "f")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"stop {GATEWAY_ID}", log)
        self.assertIn("pg_restore -U gateway -d gateway --clean", log)
        self.assertIn("--single-transaction", log)

    def test_legacy_restore_rejects_mismatched_volume_before_stopping_gateway(self):
        for mount in ("running candidate-spool", "exited apg_ledger-spool", "running "):
            with self.subTest(mount=mount):
                result, log = self.run_operation("123; 1259 456 TABLE public users gateway", "f", legacy_mount=mount)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("/var/lib/agent-pool-gateway/ledger-spool", log)
                self.assertIn("apg-gateway-1", log)
                self.assertNotIn("stop ", log)
                self.assertNotIn("pg_restore -U gateway -d gateway --clean", log)

    def test_legacy_restore_rejects_wrong_database_before_stopping_gateway(self):
        result, log = self.run_operation("123; 1259 456 TABLE public users gateway", "f", source_status="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("stop ", log)
        self.assertNotIn("pg_restore -U gateway -d gateway --clean", log)

    def test_legacy_restore_refuses_restarted_database_before_restore(self):
        result, log = self.run_operation("123; 1259 456 TABLE public users gateway", "f", postgres_started="2026-11-03T00:01:00Z")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"stop {GATEWAY_ID}", log)
        self.assertNotIn("pg_restore -U gateway -d gateway --clean", log)

    def test_legacy_restore_refuses_nonempty_spool_before_restore(self):
        result, log = self.run_operation("123; 1259 456 TABLE public users gateway", "f", spool_check_status="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("入帳目錄仍有資料", result.stderr)
        self.assertNotIn("pg_restore -U gateway -d gateway --clean", log)
        self.assertNotIn("tar -xzf", log)

    def test_legacy_restore_failure_leaves_gateway_stopped_and_reports_uncertain_database(self):
        result, log = self.run_operation("123; 1259 456 TABLE public users gateway", "f", restore_status="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("請先確認資料庫狀態", result.stderr)
        self.assertNotIn("tar -xzf", log)
        self.assertNotIn(f"start {GATEWAY_ID}", log)

    def test_spool_restore_failure_reports_database_may_have_changed(self):
        result, log = self.run_operation("123; 1259 456 TABLE public users gateway", "f", extract_status="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("資料庫可能已改變且 Gateway 已停止", result.stderr)
        self.assertIn("pg_restore -U gateway -d gateway --clean", log)
        self.assertNotIn(f"start {GATEWAY_ID}", log)

    def test_capture_backup_is_rejected_before_dump(self):
        result, log = self.run_operation("", "t", operation="create")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("safe capture backup and restore are not implemented", result.stderr)
        self.assertNotIn("pg_dump", log)

    def test_legacy_backup_rejects_gateway_using_a_different_volume_before_dump(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "docker.log"
            docker = root / "docker"
            docker.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_DOCKER_LOG\"\ncase \"$*\" in *'psql -U gateway'*) printf 'f\\n';; *'--format'*) printf 'running candidate-spool\\n';; *'pg_dump'*) printf 'synthetic-dump';; esac\nexit 0\n")
            docker.chmod(0o755)
            env = os.environ.copy()
            env.update(GATEWAY_DEPLOYMENT_LOCK=str(root / "deployment.lock"), PATH=f"{root}:{env['PATH']}", TEST_DOCKER_LOG=str(log))
            result = subprocess.run(["bash", str(SCRIPT), "create", str(root / "backups")], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("pg_dump", log.read_text())
            self.assertFalse((root / "backups").exists())

    def test_legacy_backup_rejects_wrong_database_before_dump(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "docker.log"
            docker = root / "docker"
            docker.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_DOCKER_LOG\"\ncase \"$*\" in *'psql -U gateway'*) printf 'f\\n';; *'--format'*) printf 'running apg_ledger-spool\\n';; *'pg_dump'*) printf 'synthetic-dump';; esac\nexit 0\n")
            docker.chmod(0o755)
            python = root / "python3"
            python.write_text("#!/bin/sh\nexit 1\n")
            python.chmod(0o755)
            env = os.environ.copy()
            env.update(GATEWAY_DEPLOYMENT_LOCK=str(root / "deployment.lock"), PATH=f"{root}:{env['PATH']}", TEST_DOCKER_LOG=str(log))
            result = subprocess.run(["bash", str(SCRIPT), "create", str(root / "backups")], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("pg_dump", log.read_text())
            self.assertFalse((root / "backups").exists())

    def test_legacy_backup_fails_if_source_restarts_after_dump(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "docker.log"
            counter = root / "counter"
            docker = root / "docker"
            docker.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_DOCKER_LOG\"\ncase \"$*\" in *'psql -U gateway'*) printf 'f\\n';; *'--format'*) printf 'running apg_ledger-spool\\n';; *'pg_dump'*) printf 'synthetic-dump';; esac\nexit 0\n")
            docker.chmod(0o755)
            python = root / "python3"
            changed = f"{GATEWAY_ID} {POSTGRES_ID} {STARTED} 2026-11-03T00:01:00Z 5432"
            python.write_text(f"#!/bin/sh\nn=$(cat \"$TEST_COUNTER\" 2>/dev/null || printf 0)\nn=$((n+1))\nprintf '%s' \"$n\" > \"$TEST_COUNTER\"\nif [ \"$n\" -ge 3 ]; then printf '%s\\n' '{changed}'; else printf '%s\\n' '{IDENTITY}'; fi\n")
            python.chmod(0o755)
            env = os.environ.copy()
            env.update(GATEWAY_DEPLOYMENT_LOCK=str(root / "deployment.lock"), PATH=f"{root}:{env['PATH']}", TEST_DOCKER_LOG=str(log), TEST_COUNTER=str(counter))
            result = subprocess.run(["bash", str(SCRIPT), "create", str(root / "backups")], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(f"exec -i {POSTGRES_ID} env", log.read_text())
            self.assertIn("pg_dump -U gateway -d gateway -Fc", log.read_text())
            self.assertNotIn("backup written", result.stdout)
            self.assertNotIn(" tar ", log.read_text())

    def test_legacy_backup_fails_if_capture_schema_appears_after_dump(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "docker.log"
            counter = root / "counter"
            docker = root / "docker"
            docker.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_DOCKER_LOG\"\ncase \"$*\" in *'psql -U gateway'*) n=$(cat \"$TEST_COUNTER\" 2>/dev/null || printf 0); n=$((n+1)); printf '%s' \"$n\" > \"$TEST_COUNTER\"; if [ \"$n\" -ge 3 ]; then printf 't\\n'; else printf 'f\\n'; fi;; *'--format'*) printf 'running apg_ledger-spool\\n';; *'pg_dump'*) printf 'synthetic-dump';; esac\nexit 0\n")
            docker.chmod(0o755)
            python = root / "python3"
            python.write_text(f"#!/bin/sh\nprintf '%s\\n' '{IDENTITY}'\n")
            python.chmod(0o755)
            env = os.environ.copy()
            env.update(GATEWAY_DEPLOYMENT_LOCK=str(root / "deployment.lock"), PATH=f"{root}:{env['PATH']}", TEST_DOCKER_LOG=str(log), TEST_COUNTER=str(counter))
            result = subprocess.run(["bash", str(SCRIPT), "create", str(root / "backups")], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(f"exec -i {POSTGRES_ID} env", log.read_text())
            self.assertIn("pg_dump -U gateway -d gateway -Fc", log.read_text())
            self.assertNotIn("backup written", result.stdout)
            self.assertNotIn(" tar ", log.read_text())

    def test_capture_backup_rejects_invalid_fingerprints_before_creating_destination(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            docker = root / "docker"
            docker.write_text("#!/bin/sh\ncase \"$*\" in *'psql -U gateway'*) printf 't\\n';; esac\nexit 0\n")
            docker.chmod(0o755)
            for recipient, signer in (("invalid", "B" * 40), ("A" * 40, "invalid")):
                with self.subTest(recipient=recipient, signer=signer):
                    destination = root / "backups"
                    env = os.environ.copy()
                    env.update(GATEWAY_DEPLOYMENT_LOCK=str(root / "deployment.lock"), PATH=f"{root}:{env['PATH']}", CAPTURE_BACKUP_GATEWAY="test-gateway", CAPTURE_BACKUP_RECIPIENT=recipient, CAPTURE_BACKUP_SIGNER=signer)
                    result = subprocess.run(["bash", str(SCRIPT), "create", str(destination)], env=env, capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(destination.exists())

    def test_legacy_backup_does_not_delete_existing_backups(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            backups = root / "backups"
            backups.mkdir()
            for name in ("20200101T000000Z", "20200102T000000Z", "20200103T000000Z"):
                directory = backups / name
                directory.mkdir()
                (directory / "existing-data").write_bytes(name.encode())
            docker = root / "docker"
            docker.write_text("#!/bin/sh\ncase \"$*\" in\n  *'--format'*) printf 'running apg_ledger-spool\\n';;\n  *'psql -U gateway'*) printf 'f\\n';;\n  *'pg_dump'*) printf 'synthetic-dump';;\n  *'tar -czf'*) for arg in \"$@\"; do case \"$arg\" in *:/out) printf 'synthetic-archive' > \"${arg%:/out}/ledger-spool.tgz\";; esac; done;;\nesac\nexit 0\n")
            docker.chmod(0o755)
            python = root / "python3"
            python.write_text(f"#!/bin/sh\nprintf '%s\\n' '{IDENTITY}'\nexit 0\n")
            python.chmod(0o755)
            env = os.environ.copy()
            env.update(GATEWAY_DEPLOYMENT_LOCK=str(root / "deployment.lock"), PATH=f"{root}:{env['PATH']}", BACKUP_KEEP="1")
            result = subprocess.run(["bash", str(SCRIPT), "create", str(backups)], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("backup written to", result.stdout)
            self.assertEqual(len(list(backups.iterdir())), 4)
            created = [item for item in backups.iterdir() if item.name not in ("20200101T000000Z", "20200102T000000Z", "20200103T000000Z")]
            self.assertEqual((created[0] / ".legacy-complete").read_bytes(), b"ok\n")
            for name in ("20200101T000000Z", "20200102T000000Z", "20200103T000000Z"):
                self.assertEqual((backups / name / "existing-data").read_bytes(), name.encode())

    def test_legacy_backup_rejects_existing_destination_without_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            backups = root / "backups"
            destination = backups / "20200101T000000Z"
            destination.mkdir(parents=True)
            dump = destination / "gateway.dump"
            dump.write_bytes(b"original backup")
            spool = destination / "ledger-spool.tgz"
            spool.write_bytes(b"original spool")
            log = root / "docker.log"
            docker = root / "docker"
            docker.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_DOCKER_LOG\"\ncase \"$*\" in\n  *'--format'*) printf 'running apg_ledger-spool\\n';;\n  *'psql -U gateway'*) printf 'f\\n';;\n  *'pg_dump'*) printf 'replacement';;\nesac\nexit 0\n")
            docker.chmod(0o755)
            python = root / "python3"
            python.write_text(f"#!/bin/sh\nprintf '%s\\n' '{IDENTITY}'\nexit 0\n")
            python.chmod(0o755)
            date = root / "date"
            date.write_text("#!/bin/sh\nprintf '20200101T000000Z\\n'\n")
            date.chmod(0o755)
            env = os.environ.copy()
            env.update(GATEWAY_DEPLOYMENT_LOCK=str(root / "deployment.lock"), PATH=f"{root}:{env['PATH']}", TEST_DOCKER_LOG=str(log))
            result = subprocess.run(["bash", str(SCRIPT), "create", str(backups)], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("目的地已存在", result.stderr)
            self.assertEqual(dump.read_bytes(), b"original backup")
            self.assertEqual(spool.read_bytes(), b"original spool")
            self.assertNotIn("pg_dump", log.read_text())

    def test_capture_backup_uses_encrypted_tool_then_verifies(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            backups = root / "backups"
            backups.mkdir()
            old = backups / "old-backup"
            old.mkdir()
            (old / "preserved").write_bytes(b"original")
            log = root / "commands.log"
            docker = root / "docker"
            docker.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_DOCKER_LOG\"\ncase \"$*\" in *'psql -U gateway'*) printf 'f\\n';; esac\nexit 0\n")
            docker.chmod(0o755)
            python = root / "python3"
            python.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_COMMANDS\"\ncase \"$1\" in *verify_capture_backup.py) [ \"${TEST_FAIL_VERIFY:-0}\" = 1 ] && exit 1;; esac\nexit 0\n")
            python.chmod(0o755)
            env = os.environ.copy()
            docker_log = root / "docker.log"
            env.update(COMPOSE_PROJECT="apg", GATEWAY_DEPLOYMENT_LOCK=str(root / "deployment.lock"), PATH=f"{root}:{env['PATH']}", TEST_COMMANDS=str(log), TEST_DOCKER_LOG=str(docker_log), CAPTURE_BACKUP_GATEWAY="test-gateway", CAPTURE_BACKUP_RECIPIENT="A" * 40, CAPTURE_BACKUP_SIGNER="B" * 40)
            result = subprocess.run(["bash", str(SCRIPT), "create", str(backups)], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            commands = log.read_text().splitlines()
            self.assertEqual(len(commands), 2)
            self.assertIn("capture_backup.py", commands[0])
            self.assertIn("verify_capture_backup.py", commands[1])
            backup_args = shlex.split(commands[0])
            for flag, value in (("--gateway", "test-gateway"), ("--postgres", "apg-postgres-1"), ("--recipient", "A" * 40), ("--signer", "B" * 40)):
                self.assertEqual(backup_args[backup_args.index(flag) + 1], value)
            verify_args = shlex.split(commands[1])
            self.assertEqual(verify_args[verify_args.index("--recipient") + 1], "A" * 40)
            self.assertNotIn("psql", docker_log.read_text())
            self.assertNotIn("pg_dump", result.stdout)
            env["TEST_FAIL_VERIFY"] = "1"
            failed = subprocess.run(["bash", str(SCRIPT), "create", str(backups)], env=env, capture_output=True, text=True)
            self.assertNotEqual(failed.returncode, 0)
            self.assertNotIn("加密備份已建立並驗證", failed.stdout)
            self.assertEqual((old / "preserved").read_bytes(), b"original")

    def test_incomplete_capture_configuration_never_falls_back_to_legacy_backup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            docker = root / "docker"
            docker.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_DOCKER_LOG\"\nexit 0\n")
            docker.chmod(0o755)
            log = root / "docker.log"
            env = os.environ.copy()
            for name in ("CAPTURE_BACKUP_GATEWAY", "CAPTURE_BACKUP_RECIPIENT", "CAPTURE_BACKUP_SIGNER"):
                env.pop(name, None)
            env.update(GATEWAY_DEPLOYMENT_LOCK=str(root / "deployment.lock"), PATH=f"{root}:{env['PATH']}", TEST_DOCKER_LOG=str(log), CAPTURE_BACKUP_GATEWAY="test-gateway")
            result = subprocess.run(["bash", str(SCRIPT), "create", str(root / "backups")], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("請完整設定", result.stderr)
            self.assertNotIn("psql", log.read_text())
            self.assertFalse((root / "backups").exists())


if __name__ == "__main__":
    unittest.main()
