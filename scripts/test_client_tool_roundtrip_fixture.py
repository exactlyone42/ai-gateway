from pathlib import Path
import runpy
import unittest

from scripts.client_tool_roundtrip_fixture import handler


class ClientToolRoundtripFixtureTest(unittest.TestCase):
    def exercise(self, codex, preserve_user, correct_result):
        root = Path(__file__).resolve().parent.parent
        base = runpy.run_path(str(root / 'scripts/client-roundtrip-mock.py'))['FixtureHandler']
        fixture = handler(base)
        fixture.tool_mode = True
        fixture.fixture_dir = root / 'tests/fixtures/client-roundtrip'
        fixture.context_marker = 'original-context-marker'
        fixture.expected_value = 'real-tool-value'
        fixture.nonce_path = '/isolated/fixture.txt'
        original = {'role': 'user', 'content': fixture.context_marker}

        def request(body):
            instance = object.__new__(fixture)
            instance.path = '/v1/responses' if codex else '/v1/messages'
            instance._record_path = lambda: None
            instance._capture = lambda *_: None
            instance._json_body = lambda: body
            instance._send = lambda status, *_: status
            return instance.do_POST()

        field = 'input' if codex else 'messages'
        tools = [{'type': 'function', 'name': 'exec_command'}] if codex else [{'name': 'Read'}]
        self.assertEqual(request({'model': 'fixture', field: [original], 'tools': tools}), 200)
        value = fixture.expected_value if correct_result else 'wrong-tool-value'
        output = value + ' ' + fixture.context_marker
        if codex:
            conversation = [{'type': 'function_call', 'call_id': fixture.tool_call_id, 'name': 'exec_command'}, {'type': 'function_call_output', 'call_id': fixture.tool_call_id, 'output': output}]
        else:
            conversation = [{'role': 'assistant', 'content': [{'type': 'tool_use', 'id': fixture.tool_call_id, 'name': 'Read'}]}, {'role': 'user', 'content': [{'type': 'tool_result', 'tool_use_id': fixture.tool_call_id, 'content': output}]}]
        if preserve_user:
            conversation.insert(0, original)
        result = request({'model': 'fixture', field: conversation})
        return result, fixture

    def test_original_user_and_real_tool_result_are_required(self):
        for codex in (False, True):
            with self.subTest(codex=codex):
                status, fixture = self.exercise(codex, True, True)
                self.assertEqual(status, 200)
                self.assertTrue(fixture.tool_result_received)
                self.assertTrue(fixture.context_preserved)

    def test_a_marker_in_tool_output_cannot_replace_the_original_user(self):
        for codex in (False, True):
            with self.subTest(codex=codex):
                status, fixture = self.exercise(codex, False, True)
                self.assertEqual(status, 422)
                self.assertEqual(fixture.tool_rejection, 'missing_original_user_message')
                self.assertFalse(fixture.tool_result_received)

    def test_a_wrong_tool_result_cannot_finish_the_roundtrip(self):
        for codex in (False, True):
            with self.subTest(codex=codex):
                status, fixture = self.exercise(codex, True, False)
                self.assertEqual(status, 422)
                self.assertEqual(fixture.tool_rejection, 'missing_real_tool_result_or_call')
                self.assertFalse(fixture.tool_result_received)


if __name__ == '__main__':
    unittest.main()
