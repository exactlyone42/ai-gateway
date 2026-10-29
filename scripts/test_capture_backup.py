import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


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
if args[0] == 'inspect':
    if args[1] == 'synthetic-db':
        name = '/wrong-db' if os.environ.get('TEST_WRONG_DATABASE') else '/synthetic-db'
        print(json.dumps([{'Id':'synthetic-postgres','Name':name,'State':{'Running':True},'NetworkSettings':{'Networks':{'test':{'Aliases':[], 'IPAddress':'172.18.0.3'}}}}]))
        sys.exit(0)
    destinations = ['/var/lib/agent-pool-gateway/capture-spool', '/var/lib/agent-pool-gateway/capture-keys', '/var/lib/agent-pool-gateway/ledger-spool']
    names = ['capture-test', 'keys-test', 'ledger-test']
    mounts = [{'Type':'volume', 'Name':name, 'Source':'/var/lib/docker/volumes/'+name+'/_data', 'Destination':dest} for name, dest in zip(names, destinations)]
    if os.environ.get('TEST_MISSING_MOUNT'):
        mounts.pop()
    print(json.dumps([{'Id':'synthetic-gateway', 'State':{'Running':True}, 'Mounts':mounts, 'Config':{'Env':['DATABASE_URL=postgres://gateway:synthetic@synthetic-db/gateway']}, 'NetworkSettings':{'Networks':{'test':{}}}}]))
elif args[0] == 'exec' and 'getent' in args:
    print('172.18.0.3 STREAM synthetic-db')
    if os.environ.get('TEST_AMBIGUOUS_DATABASE'):
        print('172.18.0.4 STREAM synthetic-db')
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
        cls.temp.cleanup()

    def run_backup(self, name, **extra):
        destination = self.base / name
        env = self.env.copy()
        env.update(extra)
        result = subprocess.run(["python3", str(SCRIPT), "--gateway", "synthetic", "--postgres", "synthetic-db", "--destination", str(destination), "--recipient", self.recipient, "--signer", self.signer], env=env, capture_output=True, text=True)
        return result, destination

    def test_encrypted_backup_can_be_decrypted_and_contains_no_plaintext(self):
        result, destination = self.run_backup("success")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((destination / ".complete").read_bytes(), b"ok\n")
        manifest = json.loads((destination / "manifest.json").read_text())
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

    def test_failed_source_does_not_commit_a_backup(self):
        result, destination = self.run_backup("source-failure", TEST_FAIL_SOURCE="keys-test")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((destination / ".complete").exists())

    def test_missing_mount_is_rejected_before_creating_destination(self):
        result, destination = self.run_backup("missing-mount", TEST_MISSING_MOUNT="1")
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

    def test_empty_database_dump_does_not_commit_a_backup(self):
        result, destination = self.run_backup("empty-dump", TEST_EMPTY_DUMP="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((destination / ".complete").exists())


if __name__ == "__main__":
    unittest.main()
