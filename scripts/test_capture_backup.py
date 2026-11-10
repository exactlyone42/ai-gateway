import hashlib
import contextlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock
try:
    from scripts import verify_capture_backup as verify_module
except ModuleNotFoundError:
    import verify_capture_backup as verify_module

VerifyError = verify_module.VerifyError
valid_position = verify_module.valid_position
verify_signature = verify_module.verify_signature


SCRIPT = Path(__file__).with_name("capture_backup.py")
VERIFY = Path(__file__).with_name("verify_capture_backup.py")
SENSITIVE = b"synthetic-private-capture-and-key"


class CaptureBackupTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="capture-backup-test-")
        cls.base = Path(cls.temp.name)
        cls.gpg_home = cls.base / "gnupg"
        cls.gpg_home.mkdir(mode=0o700)
        cls.env = os.environ.copy()
        cls.env.pop("DOCKER_HOST", None)
        cls.env.pop("DOCKER_CONTEXT", None)
        cls.env["GNUPGHOME"] = str(cls.gpg_home)
        subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Capture Test <capture@example.invalid>", "rsa2048", "encr", "0"], env=cls.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        listing = subprocess.run(["gpg", "--batch", "--with-colons", "--list-keys"], env=cls.env, capture_output=True, text=True, check=True).stdout
        cls.recipient = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
        subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Capture Signer <signer@example.invalid>", "rsa2048", "sign", "0"], env=cls.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        listing = subprocess.run(["gpg", "--batch", "--with-colons", "--list-secret-keys", "signer@example.invalid"], env=cls.env, capture_output=True, text=True, check=True).stdout
        cls.signer = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
        docker = cls.base / "docker"
        docker.write_text("""#!/usr/bin/python3
import io, json, os, pathlib, sys, tarfile
args = sys.argv[1:]
if os.environ.get('TEST_DOCKER_LOG'):
    with open(os.environ['TEST_DOCKER_LOG'], 'a') as log:
        log.write(' '.join(args) + '\\n')
if args[0] == 'context' and args[1] == 'show':
    print('fixture')
elif args[0] == 'context' and args[1] == 'inspect':
    host = 'tcp://fixture.invalid:2375' if os.environ.get('TEST_REMOTE_DOCKER') else 'unix:///fixture.sock'
    print(json.dumps([{'Endpoints':{'docker':{'Host':host}}}]))
elif args[0] == 'volume' and args[1] == 'inspect':
    marker = os.environ.get('TEST_VOLUME_IDENTITY_FILE')
    created = 'changed' if marker and pathlib.Path(marker).exists() else 'stable'
    if marker:
        pathlib.Path(marker).write_text('seen')
    print(json.dumps([{'Name':name,'Driver':'local','Scope':'local','CreatedAt':created,'Options':{}} for name in args[2:]]))
elif args[0] == 'inspect':
    if args[1] in ('synthetic-db', 'b' * 64):
        name = '/wrong-db' if os.environ.get('TEST_WRONG_DATABASE') else '/synthetic-db'
        aliases = []
        if os.environ.get('TEST_UPPERCASE_ALIAS'):
            name, aliases = '/other-name', ['SYNTHETIC-DB']
        ipv6 = '2001:db8::2' if os.environ.get('TEST_VALID_IPV6') else ''
        print(json.dumps([{'Id':'b'*64,'Name':name,'State':{'Running':True,'StartedAt':'stable'},'Config':{'Env':[]},'NetworkSettings':{'Networks':{'test':{'Aliases':aliases, 'IPAddress':'172.18.0.3', 'GlobalIPv6Address':ipv6}}}}]))
        sys.exit(0)
    destinations = ['/var/lib/agent-pool-gateway/capture-spool', '/var/lib/agent-pool-gateway/capture-keys', '/var/lib/agent-pool-gateway/ledger-spool', '/var/lib/agent-pool-gateway/capture-erasure', '/var/lib/agent-pool-gateway/capture-witness']
    names = ['capture-test', 'keys-test', 'ledger-test', 'erasure-test', 'witness-test']
    if os.environ.get('TEST_ANONYMOUS_ERASURE'):
        names[3] = 'a' * 64
    mounts = [{'Type':'volume', 'Name':name, 'Source':'/var/lib/docker/volumes/'+name+'/_data', 'Destination':dest} for name, dest in zip(names, destinations)]
    if os.environ.get('TEST_BIND_INDEX'):
        index = int(os.environ['TEST_BIND_INDEX']) - 1
        mounts[index] = {'Type':'bind', 'Source':'/fixture/existing', 'Destination':destinations[index]}
    if os.environ.get('TEST_MISSING_MOUNT'):
        mounts.pop()
    db_url = 'postgres://gateway:synthetic@synthetic-db:5432/gateway'
    if os.environ.get('TEST_OVERRIDE_DB_HOST'):
        db_url += '?host=other'
    if os.environ.get('TEST_WRONG_DB_PORT'):
        db_url = 'postgres://gateway:synthetic@synthetic-db:5433/gateway'
    if os.environ.get('TEST_MISSING_DB_PORT'):
        db_url = 'postgres://gateway:synthetic@synthetic-db/gateway'
    if os.environ.get('TEST_DBNAME_CONNECTION_STRING'):
        db_url = 'postgres://gateway:synthetic@synthetic-db:5432/host%3Devil'
    if os.environ.get('TEST_VALID_IPV6'):
        db_url = 'postgres://gateway:synthetic@[2001:db8::2]:5432/gateway'
    env = ['DATABASE_URL=' + db_url]
    env += [key+'='+destination for key, destination in zip(['CONTENT_CAPTURE_SPOOL_DIR', 'CONTENT_CAPTURE_KEY_DIR', 'LEDGER_SPOOL_DIR', 'CONTENT_CAPTURE_ERASURE_DIR', 'CONTENT_CAPTURE_WITNESS_DIR'], destinations)]
    if os.environ.get('TEST_WRONG_STORAGE_PATH'):
        env[-1] = 'CONTENT_CAPTURE_WITNESS_DIR=/wrong'
    started = 'changed' if os.environ.get('TEST_RESTART_GATEWAY') and args[1] == 'synthetic-gateway' else 'stable'
    print(json.dumps([{'Id':'synthetic-gateway', 'State':{'Running':True,'StartedAt':started}, 'Mounts':mounts, 'Config':{'Env':env}, 'NetworkSettings':{'Networks':{'test':{}}}}]))
elif args[0] == 'exec' and 'getent' in args:
    print('2001:db8::2 STREAM synthetic-db' if os.environ.get('TEST_VALID_IPV6') else '172.18.0.3 STREAM synthetic-db')
    if os.environ.get('TEST_AMBIGUOUS_DATABASE'):
        print('172.18.0.4 STREAM synthetic-db')
    if os.environ.get('TEST_FOREIGN_IPV6'):
        print('2001:db8::1 synthetic-db')
elif args[0] == 'exec' and '--inspect-capture-erasure' in args:
    if os.environ.get('TEST_INVALID_POSITION'):
        print('not-json')
        sys.exit(0)
    marker = os.environ.get('TEST_POSITION_FILE')
    sequence = int(bool(marker and pathlib.Path(marker).exists()))
    if marker:
        pathlib.Path(marker).write_text('seen')
    print(json.dumps({'install':'00000000-0000-4000-8000-000000000001','sequence':sequence,'tip':[0]*32}))
elif args[0] == 'exec' and 'psql' in args:
    marker = os.environ.get('TEST_IDENTITY_FILE')
    if marker and pathlib.Path(marker).exists():
        print('5678|123')
    else:
        if marker:
            pathlib.Path(marker).write_text('seen')
        print(('-1234' if os.environ.get('TEST_NEGATIVE_IDENTITY') else '1234') + '|123')
elif args[0] == 'run' and 'flock -n -x' in ' '.join(args):
    if os.environ.get('TEST_LOCK_FAILURE'):
        sys.exit(1)
    output = next(arg.split('source=',1)[1].split(',target=/backup',1)[0] for arg in args if ',target=/backup' in arg)
    print('READY', flush=True)
    if sys.stdin.readline().strip() != 'commit':
        sys.exit(1)
    marker = pathlib.Path(output) / '.complete'
    marker.write_bytes(b'ok\\n')
    marker.chmod(0o600)
elif args[0] == 'run' and '--entrypoint' in args and args[args.index('--entrypoint') + 1] == 'pg_restore':
    if not sys.stdin.buffer.read().startswith(b'PGDMP'):
        sys.exit(1)
elif args[0] == 'run' and '--entrypoint' in args and args[args.index('--entrypoint') + 1] == 'tar' and '-tf' in args:
    with tarfile.open(fileobj=sys.stdin.buffer, mode='r|') as archive:
        list(archive)
elif args[0] == 'ps' and any('gateway.backup_producer=' in arg for arg in args):
    pass
elif args[0] in ('run', 'exec'):
    if os.environ.get('TEST_FAIL_SOURCE') and os.environ['TEST_FAIL_SOURCE'] in ' '.join(args):
        sys.exit(1)
    if os.environ.get('TEST_EMPTY_DUMP') and args[0] == 'exec':
        sys.exit(0)
    if args[0] == 'exec':
        sys.stdout.buffer.write(b'PGDMPsynthetic-private-capture-and-key')
    else:
        payload = b'synthetic-private-capture-and-key'
        with tarfile.open(fileobj=sys.stdout.buffer, mode='w|') as archive:
            info = tarfile.TarInfo('fixture')
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))
else:
    sys.exit(1)
""")
        docker.chmod(0o755)
        cls.env["PATH"] = f"{cls.base}:{cls.env['PATH']}"

    @classmethod
    def tearDownClass(cls):
        subprocess.run(["gpgconf", "--kill", "gpg-agent"], env=cls.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        cls.temp.cleanup()

    def run_backup(self, name, **extra):
        destination = self.base / name
        env = self.env.copy()
        env.update(extra)
        result = subprocess.run(["python3", str(SCRIPT), "--gateway", "synthetic", "--postgres", "synthetic-db", "--destination", str(destination), "--recipient", self.recipient, "--signer", self.signer], env=env, capture_output=True, text=True)
        return result, destination

    def test_an_explicit_backup_role_is_used_for_dump_and_identity(self):
        log = self.base / "readonly-backup-commands.log"
        result, _ = self.run_backup("readonly-role", CAPTURE_BACKUP_DATABASE_USER="fixture_backup", TEST_DOCKER_LOG=str(log))
        self.assertEqual(result.returncode, 0, result.stderr)
        commands = log.read_text().splitlines()
        database = [line for line in commands if "pg_dump " in line or "psql " in line]
        self.assertGreaterEqual(len(database), 3)
        self.assertTrue(all("-U fixture_backup " in line for line in database), database)

    def test_an_invalid_backup_role_is_rejected_without_falling_back(self):
        log = self.base / "invalid-backup-role-commands.log"
        result, destination = self.run_backup("invalid-backup-role", CAPTURE_BACKUP_DATABASE_USER="fixture backup", TEST_DOCKER_LOG=str(log))
        self.assertEqual(result.returncode, 1)
        self.assertFalse(destination.exists())
        self.assertFalse(log.exists())

    def test_existing_backup_destination_is_preserved(self):
        destination = self.base / "existing-destination"
        destination.mkdir()
        marker = destination / "existing-data"
        marker.write_bytes(b"existing backup remains unchanged")
        result, _ = self.run_backup("existing-destination")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("備份目的地已存在", result.stderr)
        self.assertNotIn(str(destination), result.stderr)
        self.assertNotIn("synthetic", result.stderr)
        self.assertEqual(marker.read_bytes(), b"existing backup remains unchanged")
        self.assertEqual(sorted(path.name for path in destination.iterdir()), ["existing-data"])

    def test_database_identity_change_during_backup_prevents_completion(self):
        result, destination = self.run_backup("database-identity-change", TEST_IDENTITY_FILE=str(self.base / "identity-changed"))
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((destination / ".complete").exists())

    def test_storage_identity_change_during_backup_prevents_completion(self):
        result, destination = self.run_backup("storage-identity-change", TEST_VOLUME_IDENTITY_FILE=str(self.base / "storage-identity-changed"))
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((destination / ".complete").exists())

    def test_backup_accepts_signed_database_system_identifier(self):
        result, destination = self.run_backup("negative-database-identity", TEST_NEGATIVE_IDENTITY="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        verified = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer, "--recipient", self.recipient], env=self.env, capture_output=True, text=True)
        self.assertEqual(verified.returncode, 0, verified.stderr)

    def test_verification_requires_the_expected_recipient_when_specified(self):
        result, destination = self.run_backup("recipient-check")
        self.assertEqual(result.returncode, 0, result.stderr)
        args = ["python3", str(VERIFY), str(destination), "--signer", self.signer, "--recipient"]
        matching = subprocess.run(args + [self.recipient], env=self.env, capture_output=True, text=True)
        self.assertEqual(matching.returncode, 0, matching.stderr)
        different = subprocess.run(args + [self.signer], env=self.env, capture_output=True, text=True)
        self.assertNotEqual(different.returncode, 0)
        without_recipient = subprocess.run(args[:-1] + ["--write-receipt"], env=self.env, capture_output=True, text=True)
        self.assertNotEqual(without_recipient.returncode, 0)
        self.assertFalse((destination / "verified.sig").exists())
        receipt = subprocess.run(args + [self.recipient, "--write-receipt"], env=self.env, capture_output=True, text=True)
        self.assertEqual(receipt.returncode, 0, receipt.stderr)
        self.assertTrue((destination / "verified.sig").exists())

    def test_direct_receipt_requires_verified_backup_and_valid_recipient(self):
        result, destination = self.run_backup("direct-receipt-check")
        self.assertEqual(result.returncode, 0, result.stderr)
        encrypted = destination / "gateway.dump.gpg"
        encrypted.write_bytes(encrypted.read_bytes() + b"altered")
        code = "from scripts.verify_capture_backup import write_receipt, VerifyError; from pathlib import Path; import sys\ntry: write_receipt(Path(sys.argv[1]), sys.argv[2], sys.argv[3])\nexcept VerifyError: pass\nelse: raise SystemExit(1)"
        for recipient in (self.recipient, "", "wrong", "測試"):
            with self.subTest(recipient=recipient):
                attempt = subprocess.run(["python3", "-c", code, str(destination), self.signer, recipient], env=self.env, capture_output=True)
                self.assertEqual(attempt.returncode, 0, attempt.stderr)
                self.assertFalse((destination / "verified.sig").exists())

    def test_receipt_publication_failure_preserves_existing_files_and_can_retry(self):
        result, destination = self.run_backup("receipt-publication-failure")
        self.assertEqual(result.returncode, 0, result.stderr)
        code = "from scripts.verify_capture_backup import write_receipt; from unittest.mock import patch; from pathlib import Path; import sys\ntry:\n with patch('scripts.verify_capture_backup.os.link', side_effect=OSError('fixture')): write_receipt(Path(sys.argv[1]), sys.argv[2], sys.argv[3])\nexcept OSError: pass\nelse: raise SystemExit(1)"
        attempt = subprocess.run(["python3", "-c", code, str(destination), self.signer, self.recipient], env=self.env, capture_output=True)
        self.assertEqual(attempt.returncode, 0, attempt.stderr)
        self.assertFalse((destination / "verified.sig").exists())
        self.assertFalse(any(path.name.startswith(".verified-") for path in destination.iterdir()))
        receipt = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer, "--recipient", self.recipient, "--write-receipt"], env=self.env, capture_output=True, text=True)
        self.assertEqual(receipt.returncode, 0, receipt.stderr)
        original = (destination / "verified.sig").read_bytes()
        retry = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer, "--recipient", self.recipient, "--write-receipt"], env=self.env, capture_output=True)
        self.assertNotEqual(retry.returncode, 0)
        self.assertEqual((destination / "verified.sig").read_bytes(), original)

    def test_receipt_stays_with_the_verified_directory_if_path_is_replaced(self):
        result, destination = self.run_backup("receipt-directory-swap")
        self.assertEqual(result.returncode, 0, result.stderr)
        preserved = self.base / "receipt-directory-preserved"
        code = """from pathlib import Path
import shutil, sys
from unittest.mock import patch
from scripts import verify_capture_backup as backup
directory, preserved, signer, recipient = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3], sys.argv[4]
original = backup.verify
def swap(*args, **kwargs):
    manifest = original(*args, **kwargs)
    directory.rename(preserved)
    shutil.copytree(preserved, directory)
    encrypted = directory / 'gateway.dump.gpg'
    data = encrypted.read_bytes()
    encrypted.write_bytes(bytes([data[0] ^ 1]) + data[1:])
    return manifest
with patch.object(backup, 'verify', side_effect=swap):
    backup.write_receipt(directory, signer, recipient)
"""
        attempt = subprocess.run(["python3", "-c", code, str(destination), str(preserved), self.signer, self.recipient], env=self.env, capture_output=True)
        self.assertEqual(attempt.returncode, 0, attempt.stderr)
        self.assertTrue((preserved / "verified.sig").exists())
        self.assertFalse((destination / "verified.sig").exists())

    def test_verification_rejects_ciphertext_for_a_different_key(self):
        result, destination = self.run_backup("recipient-ciphertext-check")
        self.assertEqual(result.returncode, 0, result.stderr)
        args = ["python3", str(VERIFY), str(destination), "--signer", self.signer, "--recipient", self.recipient]
        valid = subprocess.run(args, env=self.env, capture_output=True, text=True)
        self.assertEqual(valid.returncode, 0, valid.stderr)
        subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Other Recipient <other@example.invalid>", "rsa2048", "encr", "0"], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        listing = subprocess.run(["gpg", "--batch", "--with-colons", "--list-keys", "other@example.invalid"], env=self.env, capture_output=True, text=True, check=True).stdout
        other = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
        encrypted = destination / "gateway.dump.gpg"
        ciphertext = subprocess.run(["gpg", "--batch", "--trust-model", "always", "--recipient", other, "--encrypt"], input=b"PGDMP" + SENSITIVE, env=self.env, capture_output=True, check=True).stdout
        encrypted.write_bytes(ciphertext)
        manifest_path = destination / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["sha256"][encrypted.name] = hashlib.sha256(ciphertext).hexdigest()
        manifest["size"][encrypted.name] = len(ciphertext)
        manifest_path.write_text(json.dumps(manifest))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        rejected = subprocess.run(args, env=self.env, capture_output=True, text=True)
        self.assertNotEqual(rejected.returncode, 0)
        unchecked = subprocess.run(args[:-2], env=self.env, capture_output=True, text=True)
        self.assertEqual(unchecked.returncode, 0, unchecked.stderr)

    def test_decryption_accepts_a_recipient_encryption_subkey(self):
        subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Subkey Recipient <subkey@example.invalid>", "rsa2048", "cert", "0"], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        listing = subprocess.run(["gpg", "--batch", "--with-colons", "--list-keys", "subkey@example.invalid"], env=self.env, capture_output=True, text=True, check=True).stdout
        primary = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
        subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-add-key", primary, "rsa2048", "encr", "0"], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        encrypted = self.base / "subkey-dump.gpg"
        result = subprocess.run(["gpg", "--batch", "--trust-model", "always", "--recipient", primary, "--encrypt"], input=b"PGDMP" + SENSITIVE, env=self.env, capture_output=True, check=True)
        encrypted.write_bytes(result.stdout)
        probe = subprocess.run(["python3", "-c", "from scripts.verify_capture_backup import verify_stream; import sys\nwith open(sys.argv[1], 'rb') as source: verify_stream(source, 'database', sys.argv[2])", str(encrypted), primary], env=self.env, capture_output=True, text=True)
        self.assertEqual(probe.returncode, 0, probe.stderr)

    def test_backup_does_not_add_configured_extra_recipient(self):
        subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Extra Recipient <extra@example.invalid>", "rsa2048", "encr", "0"], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        listing = subprocess.run(["gpg", "--batch", "--with-colons", "--list-keys", "extra@example.invalid"], env=self.env, capture_output=True, text=True, check=True).stdout
        extra = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
        config = self.gpg_home / "gpg.conf"
        self.assertFalse(config.exists())
        other_home = self.base / "extra-keyring"
        other_home.mkdir(mode=0o700)
        other_env = self.env.copy()
        other_env["GNUPGHOME"] = str(other_home)
        secret = subprocess.run(["gpg", "--batch", "--export-secret-keys", extra], env=self.env, capture_output=True, check=True).stdout
        subprocess.run(["gpg", "--batch", "--import"], input=secret, env=other_env, capture_output=True, check=True)
        try:
            for option in ("encrypt-to", "hidden-encrypt-to"):
                with self.subTest(option=option):
                    config.write_text(f"{option} {extra}\n")
                    result, destination = self.run_backup(f"extra-recipient-{option}")
                    self.assertEqual(result.returncode, 0, result.stderr)
                    inspected = subprocess.run(["gpg", "--batch", "--list-only", "--status-fd", "1", "--decrypt", str(destination / "gateway.dump.gpg")], env=self.env, capture_output=True, text=True, check=True)
                    recipients = [line for line in inspected.stdout.splitlines() if line.startswith("[GNUPG:] ENC_TO ")]
                    self.assertEqual(len(recipients), 1)
                    expected = subprocess.run(["gpg", "--batch", "--decrypt", str(destination / "gateway.dump.gpg")], env=self.env, capture_output=True, check=True)
                    self.assertEqual(expected.stdout, b"PGDMP" + SENSITIVE)
                    other = subprocess.run(["gpg", "--batch", "--decrypt", str(destination / "gateway.dump.gpg")], env=other_env, capture_output=True)
                    self.assertNotEqual(other.returncode, 0)
        finally:
            config.unlink()
            subprocess.run(["gpgconf", "--kill", "gpg-agent"], env=other_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)

    def test_encrypted_backup_can_be_decrypted_and_contains_no_plaintext(self):
        result, destination = self.run_backup("success")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((destination / ".complete").read_bytes(), b"ok\n")
        manifest = json.loads((destination / "manifest.json").read_text())
        self.assertEqual(manifest["version"], 7)
        self.assertEqual(manifest["erasure_position"]["sequence"], 0)
        self.assertEqual(len(manifest["sha256"]), 4)
        for name, expected_hash in manifest["sha256"].items():
            payload = (destination / name).read_bytes()
            self.assertNotIn(SENSITIVE, payload)
            self.assertEqual(hashlib.sha256(payload).hexdigest(), expected_hash)
            decoded = subprocess.run(["gpg", "--batch", "--decrypt", str(destination / name)], env=self.env, capture_output=True, check=True).stdout
            if name == "gateway.dump.gpg":
                self.assertEqual(decoded, b"PGDMP" + SENSITIVE)
            else:
                import io
                import tarfile
                with tarfile.open(fileobj=io.BytesIO(decoded)) as archive:
                    self.assertEqual(archive.extractfile("fixture").read(), SENSITIVE)
        verified = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True)
        self.assertEqual(verified.returncode, 0, verified.stderr)
        wrong_signer = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.recipient], env=self.env, capture_output=True, text=True)
        self.assertNotEqual(wrong_signer.returncode, 0)
        encrypted = destination / "gateway.dump.gpg"
        original = encrypted.read_bytes()
        encrypted.write_bytes(bytes([original[0] ^ 1]) + original[1:])
        equal_size_corruption = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True)
        self.assertNotEqual(equal_size_corruption.returncode, 0)
        encrypted.write_bytes(original)
        with (destination / "capture-keys.tar.gpg").open("ab") as output:
            output.write(b"modified")
        corrupted = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True)
        self.assertNotEqual(corrupted.returncode, 0)
        manifest["sha256"]["capture-keys.tar.gpg"] = hashlib.sha256((destination / "capture-keys.tar.gpg").read_bytes()).hexdigest()
        (destination / "manifest.json").write_text(json.dumps(manifest))
        forged = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True)
        self.assertNotEqual(forged.returncode, 0)

    def test_failed_lock_does_not_commit_a_backup(self):
        result, destination = self.run_backup("lock-failure", TEST_LOCK_FAILURE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((destination / ".complete").exists())

    def test_signed_wrong_size_rejected_and_previous_version_remains_restorable(self):
        result, destination = self.run_backup("size-compatibility")
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest_path = destination / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["size"]["gateway.dump.gpg"] += 1
        manifest_path.write_text(json.dumps(manifest))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        probe = subprocess.run(["python3", "-c", "from scripts.verify_capture_backup import verify,VerifyError; import pathlib,sys\ntry: verify(pathlib.Path(sys.argv[1]),sys.argv[2])\nexcept VerifyError as error: print(error)\nelse: raise SystemExit(1)", str(destination), self.signer], env=self.env, capture_output=True, text=True)
        self.assertEqual(probe.returncode, 0, probe.stderr)
        self.assertIn("備份大小不一致", probe.stdout)
        manifest["version"] = 6
        manifest["size"]["gateway.dump.gpg"] -= 1
        del manifest["storage_identity"]
        manifest_path.write_text(json.dumps(manifest))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        earlier = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True)
        self.assertEqual(earlier.returncode, 0, earlier.stderr)
        manifest["version"] = 5
        del manifest["database_system_id"]
        del manifest["database_oid"]
        manifest_path.write_text(json.dumps(manifest))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        previous = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True)
        self.assertEqual(previous.returncode, 0, previous.stderr)
        manifest["version"] = 4
        del manifest["postgres_id"]
        manifest_path.write_text(json.dumps(manifest))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        old_version = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True)
        self.assertEqual(old_version.returncode, 0, old_version.stderr)
        manifest["version"] = 3
        del manifest["size"]
        manifest_path.write_text(json.dumps(manifest))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        verified = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True)
        self.assertEqual(verified.returncode, 0, verified.stderr)

    def test_position_rejects_invalid_types_ranges_and_extra_fields(self):
        good = {"install": "00000000-0000-4000-8000-000000000001", "sequence": 1, "tip": [0] * 32}
        self.assertTrue(valid_position(good))
        for bad in [
            {**good, "install": "not-a-uuid"},
            {**good, "sequence": True},
            {**good, "sequence": -1},
            {**good, "sequence": 2**63},
            {**good, "tip": [0] * 31},
            {**good, "tip": [256] * 32},
            {**good, "tip": [True] * 32},
            {**good, "extra": 1},
        ]:
            self.assertFalse(valid_position(bad))

    def test_expired_or_revoked_signatures_are_rejected_even_when_cryptographically_valid(self):
        with tempfile.TemporaryDirectory(prefix="capture-signature-status-") as directory:
            path = Path(directory)
            (path / "manifest.sig").write_bytes(b"fixture")
            (path / "manifest.sig").chmod(0o600)
            fingerprint = "A" * 40
            valid = f"[GNUPG:] VALIDSIG {fingerprint} 2026-01-01 1 0 4 0 1 10 00 {fingerprint}\n"
            for status, accepted in [("GOODSIG", True), ("REVKEYSIG", False), ("EXPKEYSIG", False), ("EXPSIG", False)]:
                output = f"[GNUPG:] {status} 1234 測試\n" + valid
                result = subprocess.CompletedProcess([], 0, output.encode(), b"")
                with mock.patch.object(verify_module.subprocess, "run", return_value=result):
                    if accepted:
                        verify_signature(path, b"manifest", fingerprint)
                    else:
                        with self.assertRaises(VerifyError):
                            verify_signature(path, b"manifest", fingerprint)

    def test_real_revoked_signing_key_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="capture-signature-revoked-") as directory, contextlib.ExitStack() as cleanups:
            root = Path(directory)
            home = root / "gnupg"
            home.mkdir(mode=0o700)
            env = os.environ.copy()
            env["GNUPGHOME"] = str(home)
            cleanups.callback(subprocess.run, ["gpgconf", "--kill", "gpg-agent"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
            subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Revoked Fixture <revoked@example.invalid>", "rsa2048", "sign", "0"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            listing = subprocess.run(["gpg", "--batch", "--with-colons", "--list-secret-keys"], env=env, capture_output=True, text=True, check=True).stdout
            fingerprint = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
            manifest = root / "manifest.json"
            manifest.write_bytes(b"fixture")
            signature = root / "manifest.sig"
            subprocess.run(["gpg", "--batch", "--local-user", fingerprint, "--detach-sign", "--output", str(signature), str(manifest)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            signature.chmod(0o600)
            with mock.patch.dict(os.environ, {"GNUPGHOME": str(home)}):
                verify_signature(root, b"fixture", fingerprint)
            revocation = (home / "openpgp-revocs.d" / (fingerprint + ".rev")).read_bytes()
            offset = revocation.index(b":-----BEGIN PGP PUBLIC KEY BLOCK-----")
            subprocess.run(["gpg", "--batch", "--import"], input=revocation[offset + 1:], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            status = subprocess.run(["gpg", "--batch", "--status-fd", "1", "--verify", str(signature), str(manifest)], env=env, capture_output=True, check=False)
            self.assertIn(b"[GNUPG:] REVKEYSIG ", status.stdout)
            self.assertIn(b"[GNUPG:] VALIDSIG ", status.stdout)
            with mock.patch.dict(os.environ, {"GNUPGHOME": str(home)}):
                with self.assertRaises(VerifyError):
                    verify_signature(root, b"fixture", fingerprint)

    def test_changed_erasure_position_does_not_commit_a_backup(self):
        result, destination = self.run_backup("changed-position", TEST_POSITION_FILE=str(self.base / "position-seen"))
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((destination / ".complete").exists())

    def test_restarted_gateway_does_not_commit_a_backup(self):
        result, destination = self.run_backup("restarted-gateway", TEST_RESTART_GATEWAY="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((destination / ".complete").exists())

    def test_signed_legacy_backup_and_altered_position_are_rejected(self):
        result, destination = self.run_backup("legacy-version")
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest_path = destination / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["erasure_position"]["sequence"] = 1
        manifest_path.write_text(json.dumps(manifest))
        altered = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True)
        self.assertNotEqual(altered.returncode, 0)
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        (destination / "manifest.sig").chmod(0o600)
        resigned = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True)
        self.assertEqual(resigned.returncode, 0, resigned.stderr)
        manifest["version"] = 2
        manifest_path.write_text(json.dumps(manifest))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        (destination / "manifest.sig").chmod(0o600)
        legacy = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True)
        self.assertNotEqual(legacy.returncode, 0)

    def test_legacy_v2_can_be_inspected_but_not_certified_for_restore(self):
        result, destination = self.run_backup("legacy-inspection")
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest_path = destination / "manifest.json"
        current = json.loads(manifest_path.read_text())
        manifest = {key: current[key] for key in ("created_utc", "gateway_id", "recipient_fingerprint", "signer_fingerprint", "sha256")}
        manifest["version"] = 2
        manifest_path.write_text(json.dumps(manifest))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        before = {path.name: path.read_bytes() for path in destination.iterdir()}
        args = ["python3", str(VERIFY), str(destination), "--signer", self.signer, "--recipient", self.recipient]
        inspection = subprocess.run(args + ["--inspect-legacy-v2"], env=self.env, capture_output=True, text=True)
        self.assertEqual(inspection.returncode, 0, inspection.stderr)
        self.assertIn("不能直接還原", inspection.stdout)
        self.assertEqual(before, {path.name: path.read_bytes() for path in destination.iterdir()})
        self.assertNotEqual(subprocess.run(args, env=self.env, capture_output=True).returncode, 0)
        self.assertNotEqual(subprocess.run(args + ["--inspect-legacy-v2", "--write-receipt"], env=self.env, capture_output=True).returncode, 0)
        self.assertFalse((destination / "verified.sig").exists())
        direct = subprocess.run(["python3", "-c", "from scripts.verify_capture_backup import write_receipt, VerifyError; from pathlib import Path; import sys\ntry: write_receipt(Path(sys.argv[1]), sys.argv[2], sys.argv[3])\nexcept VerifyError: pass\nelse: raise SystemExit(1)", str(destination), self.signer, self.recipient], env=self.env, capture_output=True)
        self.assertEqual(direct.returncode, 0, direct.stderr)
        self.assertFalse((destination / "verified.sig").exists())
        log = self.base / "legacy-restore-docker.log"
        restore_env = dict(self.env, TEST_DOCKER_LOG=str(log))
        restore = ["python3", str(Path(__file__).with_name("restore_capture_backup.py")), "--backup", str(destination), "--signer", self.signer, "--postgres", "fixture-db", "--spool", "fixture-spool", "--keys", "fixture-keys", "--ledger", "fixture-ledger", "--anchors", "fixture-anchors"]
        self.assertNotEqual(subprocess.run(restore, env=restore_env, capture_output=True).returncode, 0)
        self.assertFalse(log.exists())
        wrong = ["python3", str(VERIFY), str(destination), "--signer", self.signer, "--recipient", self.signer, "--inspect-legacy-v2"]
        self.assertNotEqual(subprocess.run(wrong, env=self.env, capture_output=True).returncode, 0)
        wrong_signer = ["python3", str(VERIFY), str(destination), "--signer", self.recipient, "--inspect-legacy-v2"]
        self.assertNotEqual(subprocess.run(wrong_signer, env=self.env, capture_output=True).returncode, 0)
        manifest["version"] = 3
        manifest_path.write_text(json.dumps(manifest))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        self.assertNotEqual(subprocess.run(args + ["--inspect-legacy-v2"], env=self.env, capture_output=True).returncode, 0)
        manifest["version"] = 2.0
        manifest_path.write_text(json.dumps(manifest))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        self.assertNotEqual(subprocess.run(args + ["--inspect-legacy-v2"], env=self.env, capture_output=True).returncode, 0)
        manifest["version"] = 2
        manifest_path.write_text(json.dumps(manifest))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        encrypted = destination / "gateway.dump.gpg"
        encrypted.write_bytes(encrypted.read_bytes() + b"altered")
        self.assertNotEqual(subprocess.run(args + ["--inspect-legacy-v2"], env=self.env, capture_output=True).returncode, 0)

    def test_ledger_backup_inspection_is_separate_from_capture_restore(self):
        result, source = self.run_backup("ledger-profile-source")
        self.assertEqual(result.returncode, 0, result.stderr)
        destination = self.base / "ledger-profile-inspection"
        destination.mkdir(mode=0o700)
        for name in ("gateway.dump.gpg", "ledger-spool.tar.gpg", "manifest.json", "manifest.sig", ".complete"):
            shutil.copy2(source / name, destination / name)
        source_manifest = source / "manifest.json"
        spoofed = json.loads(source_manifest.read_text())
        spoofed["profile"] = "ledger"
        source_manifest.write_text(json.dumps(spoofed))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(source / "manifest.sig"), str(source_manifest)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        self.assertNotEqual(subprocess.run(["python3", str(VERIFY), str(source), "--signer", self.signer], env=self.env, capture_output=True).returncode, 0)
        manifest_path = destination / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        files = ("gateway.dump.gpg", "ledger-spool.tar.gpg")
        manifest.update(version=8, profile="ledger")
        manifest.pop("erasure_position")
        manifest["sha256"] = {name: manifest["sha256"][name] for name in files}
        manifest["size"] = {name: manifest["size"][name] for name in files}
        manifest_path.write_text(json.dumps(manifest))
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--detach-sign", "--output", str(destination / "manifest.sig"), str(manifest_path)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        args = ["python3", str(VERIFY), str(destination), "--signer", self.signer, "--recipient", self.recipient]
        inspected = subprocess.run(args + ["--inspect-ledger-v8"], env=self.env, capture_output=True, text=True)
        self.assertEqual(inspected.returncode, 0, inspected.stderr)
        self.assertIn("不能直接還原", inspected.stdout)
        self.assertIn("不確認資料庫內容", inspected.stdout)
        extra = destination / "capture-spool.tar.gpg"
        shutil.copy2(source / extra.name, extra)
        self.assertNotEqual(subprocess.run(args + ["--inspect-ledger-v8"], env=self.env, capture_output=True).returncode, 0)
        extra.rename(self.base / "preserved-capture-spool.gpg")
        self.assertNotEqual(subprocess.run(args, env=self.env, capture_output=True).returncode, 0)
        self.assertNotEqual(subprocess.run(args + ["--inspect-ledger-v8", "--write-receipt"], env=self.env, capture_output=True).returncode, 0)
        self.assertNotEqual(subprocess.run(args + ["--inspect-ledger-v8", "--inspect-legacy-v2"], env=self.env, capture_output=True).returncode, 0)
        direct = subprocess.run(["python3", "-c", "from scripts.verify_capture_backup import write_receipt, VerifyError; from pathlib import Path; import sys\ntry: write_receipt(Path(sys.argv[1]), sys.argv[2], sys.argv[3])\nexcept VerifyError: pass\nelse: raise SystemExit(1)", str(destination), self.signer, self.recipient], env=self.env, capture_output=True)
        self.assertEqual(direct.returncode, 0, direct.stderr)
        wrong_signer = ["python3", str(VERIFY), str(destination), "--signer", self.recipient, "--inspect-ledger-v8"]
        self.assertNotEqual(subprocess.run(wrong_signer, env=self.env, capture_output=True).returncode, 0)
        wrong_recipient = ["python3", str(VERIFY), str(destination), "--signer", self.signer, "--recipient", self.signer, "--inspect-ledger-v8"]
        self.assertNotEqual(subprocess.run(wrong_recipient, env=self.env, capture_output=True).returncode, 0)
        self.assertFalse((destination / "verified.sig").exists())
        log = self.base / "ledger-profile-restore-docker.log"
        restore_env = dict(self.env, TEST_DOCKER_LOG=str(log))
        restore = ["python3", str(Path(__file__).with_name("restore_capture_backup.py")), "--backup", str(destination), "--signer", self.signer, "--postgres", "fixture-db", "--spool", "fixture-spool", "--keys", "fixture-keys", "--ledger", "fixture-ledger", "--anchors", "fixture-anchors"]
        self.assertNotEqual(subprocess.run(restore, env=restore_env, capture_output=True).returncode, 0)
        self.assertFalse(log.exists())
        spool = destination / "ledger-spool.tar.gpg"
        spool_payload = spool.read_bytes()
        spool.write_bytes(bytes([spool_payload[0] ^ 1]) + spool_payload[1:])
        self.assertEqual(spool.stat().st_size, len(spool_payload))
        self.assertNotEqual(subprocess.run(args + ["--inspect-ledger-v8"], env=self.env, capture_output=True).returncode, 0)
        spool.write_bytes(spool_payload)
        encrypted = destination / "gateway.dump.gpg"
        payload = encrypted.read_bytes()
        encrypted.write_bytes(bytes([payload[0] ^ 1]) + payload[1:])
        self.assertEqual(encrypted.stat().st_size, len(payload))
        self.assertNotEqual(subprocess.run(args + ["--inspect-ledger-v8"], env=self.env, capture_output=True).returncode, 0)

    def test_attached_signature_over_other_content_cannot_authenticate_manifest(self):
        result, destination = self.run_backup("attached-signature")
        self.assertEqual(result.returncode, 0, result.stderr)
        signature = destination / "manifest.sig"
        subprocess.run(["gpg", "--batch", "--yes", "--local-user", self.signer, "--sign", "--output", str(signature)], input=b"unrelated signed content", env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        signature.chmod(0o600)
        self.assertEqual(subprocess.run(["gpg", "--batch", "--verify", str(signature)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode, 0)
        verified = subprocess.run(["python3", str(VERIFY), str(destination), "--signer", self.signer], env=self.env, capture_output=True, text=True)
        self.assertNotEqual(verified.returncode, 0)

    def test_failed_source_does_not_commit_a_backup(self):
        result, destination = self.run_backup("source-failure", TEST_FAIL_SOURCE="keys-test")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((destination / ".complete").exists())

    def test_missing_mount_is_rejected_before_creating_destination(self):
        result, destination = self.run_backup("missing-mount", TEST_MISSING_MOUNT="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(destination.exists())

    def test_bind_mount_is_rejected_before_creating_destination(self):
        for index in range(1, 6):
            with self.subTest(index=index):
                result, destination = self.run_backup(f"unsupported-bind-{index}", TEST_BIND_INDEX=str(index))
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(destination.exists())

    def test_remote_docker_is_rejected_before_creating_destination(self):
        result, destination = self.run_backup("unsupported-remote", TEST_REMOTE_DOCKER="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(destination.exists())

    def test_remote_host_with_local_context_is_rejected_before_creating_destination(self):
        result, destination = self.run_backup("unsupported-host-override", DOCKER_CONTEXT="default", DOCKER_HOST="tcp://127.0.0.1:1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(destination.exists())

    def test_wrong_database_is_rejected_before_creating_destination(self):
        result, destination = self.run_backup("wrong-database", TEST_WRONG_DATABASE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(destination.exists())

    def test_ambiguous_database_name_is_rejected(self):
        result, destination = self.run_backup("ambiguous-database", TEST_AMBIGUOUS_DATABASE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(destination.exists())

    def test_storage_environment_must_match_mounted_paths(self):
        result, destination = self.run_backup("wrong-storage-path", TEST_WRONG_STORAGE_PATH="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(destination.exists())

    def test_database_host_override_and_wrong_port_are_rejected(self):
        for name, option in [("override-db-host", "TEST_OVERRIDE_DB_HOST"), ("wrong-db-port", "TEST_WRONG_DB_PORT"), ("missing-db-port", "TEST_MISSING_DB_PORT"), ("foreign-ipv6", "TEST_FOREIGN_IPV6"), ("connection-string-dbname", "TEST_DBNAME_CONNECTION_STRING")]:
            result, destination = self.run_backup(name, **{option: "1"})
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(destination.exists())

    def test_matching_ipv6_and_case_insensitive_alias_are_accepted(self):
        for name, option in [("valid-ipv6", "TEST_VALID_IPV6"), ("uppercase-alias", "TEST_UPPERCASE_ALIAS")]:
            result, destination = self.run_backup(name, **{option: "1"})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((destination / ".complete").exists())

    def test_anonymous_erasure_volume_is_rejected(self):
        result, destination = self.run_backup("anonymous-erasure", TEST_ANONYMOUS_ERASURE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(destination.exists())

    def test_malformed_erasure_position_does_not_commit_a_backup(self):
        result, destination = self.run_backup("invalid-position", TEST_INVALID_POSITION="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((destination / ".complete").exists())

    def test_empty_database_dump_does_not_commit_a_backup(self):
        result, destination = self.run_backup("empty-dump", TEST_EMPTY_DUMP="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((destination / ".complete").exists())


if __name__ == "__main__":
    unittest.main()
