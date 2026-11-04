import json
from pathlib import Path
import tempfile
import unittest

from sync_codex_auth import sync


class SyncTests(unittest.TestCase):
    def test_missing_then_created_replaced_and_removed(self):
        with tempfile.TemporaryDirectory() as root:
            source = Path(root) / 'source.json'
            destination = Path(root) / 'held' / 'auth.json'
            sync(source, destination, None)
            self.assertEqual(json.loads(destination.read_text()), {})
            auth = {'auth_mode': 'chatgpt', 'tokens': {
                'access_token': 'first', 'account_id': 'owner', 'refresh_token': 'private'}}
            source.write_text(json.dumps(auth))
            source.chmod(0o600)
            sync(source, destination, None)
            self.assertNotIn('refresh_token', destination.read_text())
            self.assertEqual(destination.stat().st_mode & 0o777, 0o600)
            replacement = source.with_suffix('.next')
            auth['tokens']['access_token'] = 'second'
            replacement.write_text(json.dumps(auth))
            replacement.chmod(0o600)
            replacement.replace(source)
            sync(source, destination, None)
            self.assertEqual(json.loads(destination.read_text())['tokens']['access_token'], 'second')
            source.chmod(0o644)
            sync(source, destination, None)
            self.assertEqual(json.loads(destination.read_text()), {})
            source.unlink()
            sync(source, destination, None)
            self.assertEqual(json.loads(destination.read_text()), {})


if __name__ == '__main__':
    unittest.main()
