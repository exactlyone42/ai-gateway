import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location('check_migrations', ROOT / 'check-migrations.py')
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class GuardTests(unittest.TestCase):
    def test_changed_or_missing_published_migration_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / '20261021153500_test.sql'
            target.write_bytes(b'SELECT 1;\n')
            baseline = {target.name: hashlib.sha384(target.read_bytes()).hexdigest()}
            (root / 'applied-checksums.json').write_text(json.dumps(baseline))
            CHECK.check(root)
            target.write_bytes(b'SELECT 2;\n')
            with self.assertRaises(RuntimeError):
                CHECK.check(root)
            target.unlink()
            with self.assertRaises(RuntimeError):
                CHECK.check(root)

    def test_compose_blocks_gateway_replacement_and_keeps_database_operations(self):
        for args, expected in [(['up', '-d'], 2), (['up', '-d', 'gateway'], 2),
                               (['down'], 2), (['restart'], 2),
                               (['up', '-d', 'postgres'], 0), (['ps'], 0)]:
            with self.subTest(args=args):
                result = subprocess.run(['bash', str(ROOT / 'check-safe-compose.sh'), *args],
                                        capture_output=True, timeout=5)
                self.assertEqual(result.returncode, expected)


if __name__ == '__main__':
    unittest.main()
