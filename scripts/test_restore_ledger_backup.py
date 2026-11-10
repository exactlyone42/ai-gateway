import io
import os
from pathlib import Path
import sys
import subprocess
import tarfile
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
from restore_ledger_backup import RestoreError, check_ledger_archive
from capture_backup import BackupError, encrypt_stream
from ledger_backup_bundle import write_bundle
from verify_capture_backup import VerifyError


class ArchiveTest(unittest.TestCase):
    def test_stalled_encryptor_stops_when_coordinator_exits(self):
        with tempfile.TemporaryDirectory(prefix="ledger-stalled-encrypt-") as directory:
            root = Path(directory)
            fake_gpg = root / "gpg"
            fake_gpg.write_text("#!" + sys.executable + "\nimport os,signal,time\nfrom pathlib import Path\nsignal.signal(signal.SIGTERM,signal.SIG_IGN)\nPath(os.environ['LEDGER_TEST_GPG_PID']).write_text(str(os.getpid()))\ntime.sleep(60)\n")
            fake_gpg.chmod(0o700)
            lock = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(0.5)"])
            producer = [sys.executable, "-c", "import os\nwhile True: os.write(1,b'x'*65536)"]
            started = time.monotonic()
            try:
                with mock.patch.dict(os.environ, {"PATH": directory + os.pathsep + os.environ.get("PATH", ""), "LEDGER_TEST_GPG_PID": str(root / "gpg.pid")}):
                    with self.assertRaises(BackupError):
                        encrypt_stream(producer, root / "backup.gpg", "a" * 40, lock, None)
                self.assertLess(time.monotonic() - started, 15)
                self.assertFalse((Path("/proc") / (root / "gpg.pid").read_text()).exists())
            finally:
                if lock.poll() is None:
                    lock.terminate()
                lock.wait(timeout=10)

    def test_bundle_rejects_stopped_coordination_before_creating_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "backup"
            lock = mock.Mock()
            lock.poll.return_value = 1
            identity = {"gateway_id": "a" * 64, "postgres_id": "b" * 64, "database_system_id": "1", "database_oid": "1", "storage_identity": "c" * 64}
            with self.assertRaises(BackupError):
                write_bundle(destination, {"gateway.dump.gpg": ["true"], "ledger-spool.tar.gpg": ["true"]}, "a" * 40, "b" * 40, identity, lock)
            self.assertFalse(destination.exists())

    def test_bundle_does_not_complete_if_commit_coordinator_dies(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "backup"
            lock = mock.Mock()
            lock.poll.return_value = None
            lock.stdin.write.side_effect = BrokenPipeError()
            identity = {"gateway_id": "a" * 64, "postgres_id": "b" * 64, "database_system_id": "1", "database_oid": "1", "storage_identity": "c" * 64}
            def encrypt(producer, path, recipient, coordinator, prefix):
                path.write_bytes(b"encrypted")
                path.chmod(0o600)
            with mock.patch("ledger_backup_bundle.encrypt_stream", side_effect=encrypt), mock.patch("ledger_backup_bundle.subprocess.run", return_value=mock.Mock(returncode=0)), mock.patch("ledger_backup_bundle.verify_signature"):
                with self.assertRaises(BackupError):
                    write_bundle(destination, {"gateway.dump.gpg": ["true"], "ledger-spool.tar.gpg": ["true"]}, "a" * 40, "b" * 40, identity, lock)
            self.assertFalse((destination / ".complete").exists())

    def test_bundle_detects_ciphertext_changed_before_completion(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "backup"
            lock = mock.Mock()
            lock.poll.return_value = None
            identity = {"gateway_id": "a" * 64, "postgres_id": "b" * 64, "database_system_id": "1", "database_oid": "1", "storage_identity": "c" * 64}
            def encrypt(producer, path, recipient, coordinator, prefix):
                path.write_bytes(b"encrypted")
                path.chmod(0o600)
            def alter():
                (destination / "gateway.dump.gpg").write_bytes(b"corrupted")
            with mock.patch("ledger_backup_bundle.encrypt_stream", side_effect=encrypt), mock.patch("ledger_backup_bundle.subprocess.run", return_value=mock.Mock(returncode=0)), mock.patch("ledger_backup_bundle.verify_signature"):
                with self.assertRaises(BackupError):
                    write_bundle(destination, {"gateway.dump.gpg": ["true"], "ledger-spool.tar.gpg": ["true"]}, "a" * 40, "b" * 40, identity, lock, final_check=alter)
            self.assertFalse((destination / ".complete").exists())

    def test_bundle_rejects_manifest_change_or_invalid_signature(self):
        for invalid_signature in (False, True):
            with self.subTest(invalid_signature=invalid_signature), tempfile.TemporaryDirectory() as directory:
                destination = Path(directory) / "backup"
                lock = mock.Mock()
                lock.poll.return_value = None
                identity = {"gateway_id": "a" * 64, "postgres_id": "b" * 64, "database_system_id": "1", "database_oid": "1", "storage_identity": "c" * 64}
                def encrypt(producer, path, recipient, coordinator, prefix):
                    path.write_bytes(b"encrypted")
                    path.chmod(0o600)
                def alter():
                    if not invalid_signature:
                        (destination / "manifest.json").write_bytes(b"tampered")
                failure = VerifyError("bad signature") if invalid_signature else None
                with mock.patch("ledger_backup_bundle.encrypt_stream", side_effect=encrypt), mock.patch("ledger_backup_bundle.subprocess.run", return_value=mock.Mock(returncode=0)), mock.patch("ledger_backup_bundle.verify_signature", side_effect=failure):
                    with self.assertRaises(BackupError):
                        write_bundle(destination, {"gateway.dump.gpg": ["true"], "ledger-spool.tar.gpg": ["true"]}, "a" * 40, "b" * 40, identity, lock, final_check=alter)
                self.assertFalse((destination / ".complete").exists())

    def test_bundle_rejects_archive_different_from_original_source(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "backup"
            lock = mock.Mock()
            lock.poll.return_value = None
            identity = {"gateway_id": "a" * 64, "postgres_id": "b" * 64, "database_system_id": "1", "database_oid": "1", "storage_identity": "c" * 64}
            def encrypt(producer, path, recipient, coordinator, prefix):
                path.write_bytes(b"encrypted")
                return "different"
            with mock.patch("ledger_backup_bundle.encrypt_stream", side_effect=encrypt):
                with self.assertRaises(BackupError):
                    write_bundle(destination, {"gateway.dump.gpg": ["true"], "ledger-spool.tar.gpg": ["true"]}, "a" * 40, "b" * 40, identity, lock, expected_hashes={"ledger-spool.tar.gpg": "original"})
            self.assertFalse((destination / ".complete").exists())

    def archive(self, name, kind=tarfile.REGTYPE, body=b"data"):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w") as archive:
            item = tarfile.TarInfo(name)
            item.type = kind
            item.size = len(body) if kind == tarfile.REGTYPE else 0
            archive.addfile(item, io.BytesIO(body) if item.size else None)
        return data.getvalue()

    def check(self, name, kind=tarfile.REGTYPE, body=b"data", limit=100):
        with tempfile.TemporaryDirectory() as directory:
            encrypted = Path(directory) / "ledger"
            encrypted.write_bytes(b"fixture")
            encrypted.chmod(0o600)
            process = mock.Mock()
            process.stdout = io.BytesIO(self.archive(name, kind, body))
            process.wait.return_value = 0
            process.poll.return_value = 0
            with mock.patch("restore_ledger_backup.subprocess.Popen", return_value=process):
                check_ledger_archive(encrypted, limit)

    def test_valid_and_unsafe_archives(self):
        self.check("a" * 32 + ".ready")
        self.check(".ledger-spool.lock", body=b"")
        for name, kind, body, limit in (("../escape", tarfile.REGTYPE, b"x", 100), ("unknown", tarfile.REGTYPE, b"x", 100), ("a" * 32 + ".ready", tarfile.SYMTYPE, b"", 100), (".ledger-spool.lock", tarfile.REGTYPE, b"x", 100), ("a" * 32 + ".ready", tarfile.REGTYPE, b"xx", 1)):
            with self.subTest(name=name), self.assertRaises(RestoreError):
                self.check(name, kind, body, limit)


if __name__ == "__main__":
    unittest.main()
