import json
from email.message import Message
from pathlib import Path
import runpy
import tempfile
import unittest


class FixtureCredentialTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        handler = runpy.run_path(str(Path(__file__).with_name('client-roundtrip-mock.py')))['FixtureHandler']
        handler.capture_dir = Path(self.directory.name)
        handler.provider_keys_by_model = {'claude-alice': 'alice-fixture', 'claude-bob': 'bob-fixture'}
        handler.provider_credentials = {'alice-fixture', 'bob-fixture', 'revoked-fixture'}
        handler.client_credentials = {'gateway-fixture'}
        handler.captured_requests = []
        self.handler = object.__new__(handler)
        self.handler.path = '/v1/messages'
        self.handler.headers = Message()
        self.handler.headers['x-api-key'] = 'alice-fixture'

    def capture(self, **extra):
        self.handler._capture('claude-code', {'model': 'claude-alice', 'stream': True, **extra})
        return self.handler.captured_requests[-1]

    def test_exact_api_key_and_bearer_are_accepted(self):
        self.assertTrue(self.capture()['provider_credential_matches_fixture'])
        self.handler.path = '/v1/responses'
        del self.handler.headers['x-api-key']
        self.handler.headers['authorization'] = 'Bearer alice-fixture'
        self.assertTrue(self.capture()['provider_credential_matches_fixture'])

    def test_other_credential_header_and_duplicate_are_rejected(self):
        for header in ('authorization', 'x-api-key', 'cookie'):
            with self.subTest(header=header):
                self.handler.headers[header] = 'bob-fixture'
                self.assertFalse(self.capture()['provider_credential_matches_fixture'])
                del self.handler.headers[header]
                if header == 'x-api-key':
                    self.handler.headers['x-api-key'] = 'alice-fixture'

    def test_other_provider_and_gateway_keys_in_body_are_rejected(self):
        self.assertFalse(self.capture(extra='bob-fixture')['provider_credential_matches_fixture'])
        self.assertFalse(self.capture(extra='revoked-fixture')['provider_credential_matches_fixture'])
        self.assertFalse(self.capture(extra='gateway-fixture')['client_credential_absent'])

    def test_capture_history_preserves_an_earlier_bad_request(self):
        self.handler.headers['authorization'] = 'Bearer bob-fixture'
        self.capture()
        del self.handler.headers['authorization']
        self.capture()
        self.assertEqual([value['provider_credential_matches_fixture'] for value in self.handler.captured_requests], [False, True])
        saved = json.loads((self.handler.capture_dir / 'claude-code.json').read_text())
        self.assertTrue(saved['provider_credential_matches_fixture'])


if __name__ == '__main__':
    unittest.main()
