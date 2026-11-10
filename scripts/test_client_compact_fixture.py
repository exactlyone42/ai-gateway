import json
from pathlib import Path
import runpy
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from urllib.request import Request, urlopen


class CompactFixtureTests(unittest.TestCase):
    def setUp(self):
        root = Path(__file__).resolve().parent.parent
        base = runpy.run_path(str(root / 'scripts/client-roundtrip-mock.py'))['FixtureHandler']
        self.fixture = runpy.run_path(str(root / 'scripts/client_compact_fixture.py'))['handler'](base)
        self.directory = tempfile.TemporaryDirectory()
        self.fixture.fixture_dir = root / 'tests/fixtures/client-roundtrip'
        self.fixture.capture_dir = Path(self.directory.name)
        self.fixture.captured_requests = []
        self.fixture.request_paths = {'claude-code': [], 'codex-cli': []}
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), self.fixture)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        self.directory.cleanup()

    def request(self, messages):
        data = json.dumps({'model': 'claude-test', 'messages': messages}).encode()
        request = Request(f'http://127.0.0.1:{self.server.server_port}/v1/messages', data=data, headers={'content-type': 'application/json', 'x-api-key': 'fixture-provider-key'})
        with urlopen(request, timeout=2) as response:
            return response.read().decode()

    def test_environment_summary_mentions_do_not_trigger_compaction(self):
        payload = self.request([
            {'role': 'user', 'content': [{'type': 'text', 'text': 'Reply exactly W05_CLAUDE_CODE_OK.'}]},
            {'role': 'system', 'content': [{'type': 'text', 'text': 'Use summaries for long sessions.'}]},
        ])
        self.assertEqual(self.fixture.compact_requests, 0)
        self.assertIn('W05_CLAUDE_CODE_OK', payload)

    def test_compact_summary_and_continuation_are_distinct(self):
        payload = self.request([{'role': 'user', 'content': [{'type': 'text', 'text': 'Your task is to create a detailed summary of the conversation so far.'}]}])
        self.assertIn('<summary>' + self.fixture.summary + '</summary>', payload)
        payload = self.request([{'role': 'user', 'content': [{'type': 'text', 'text': 'This session is being continued. The summary below covers earlier context. ' + self.fixture.summary + ' Resume the reply task.'}]}])
        self.assertEqual(self.fixture.compact_requests, 1)
        self.assertEqual(self.fixture.resumed_requests, 1)
        self.assertIn('W05_CLAUDE_CODE_OK', payload)

    def test_removed_summary_cannot_count_as_preserved_context(self):
        self.fixture.omit_summary = True
        self.request([{'role': 'user', 'content': [{'type': 'text', 'text': 'Your task is to create a detailed summary of the conversation so far.'}]}])
        self.request([{'role': 'user', 'content': [{'type': 'text', 'text': 'Continue from SUMMARY_REMOVED_CONTROL.'}]}])
        self.assertEqual(self.fixture.compact_requests, 1)
        self.assertEqual(self.fixture.resumed_requests, 0)
