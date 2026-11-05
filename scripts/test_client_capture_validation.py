import copy
import json
from types import SimpleNamespace
import unittest

from scripts.client_capture_validation import validate_tools, validate_usage, reject_plaintext


class CaptureValidationTest(unittest.TestCase):
    def sample(self, codex):
        original = {'role': 'user', 'content': 'original-marker'}
        fixture = SimpleNamespace(emitted_streams=['tool-call\ncompleted', 'reply\ncompleted'], original_user_messages=[original], tool_call_id='call-1', expected_value='real-value')
        output = {'type': 'function_call_output', 'call_id': 'call-1', 'output': 'real-value original-marker'} if codex else {'role': 'user', 'content': [{'type': 'tool_result', 'tool_use_id': 'call-1', 'content': 'real-value original-marker'}]}
        field = 'input' if codex else 'messages'
        saved = []
        for index in range(2):
            request = json.dumps({field: [original] + ([output] if index else [])})
            saved.append({'client_request': request, 'provider_request': request, 'provider_response': fixture.emitted_streams[index], 'client_response': fixture.emitted_streams[index]})
        return saved, fixture

    def test_complete_roundtrip(self):
        for codex in (False, True):
            saved, fixture = self.sample(codex)
            validate_tools(saved, fixture, codex)

    def test_equal_truncated_streams_are_rejected(self):
        for codex in (False, True):
            saved, fixture = self.sample(codex)
            saved[0]['provider_response'] = saved[0]['client_response'] = 'tool-call'
            with self.assertRaisesRegex(RuntimeError, '串流'):
                validate_tools(saved, fixture, codex)

    def test_marker_in_result_cannot_replace_user(self):
        for codex in (False, True):
            for leg in ('client_request', 'provider_request'):
                saved, fixture = self.sample(codex)
                body = json.loads(saved[1][leg])
                body['input' if codex else 'messages'].pop(0)
                saved[1][leg] = json.dumps(body)
                with self.assertRaisesRegex(RuntimeError, '完整原訊息'):
                    validate_tools(saved, fixture, codex)

    def test_wrong_tool_result_is_rejected(self):
        for codex in (False, True):
            saved, fixture = self.sample(codex)
            changed = copy.deepcopy(saved)
            changed[1]['provider_request'] = changed[1]['provider_request'].replace('real-value', 'wrong-value')
            with self.assertRaisesRegex(RuntimeError, '真實工具結果'):
                validate_tools(changed, fixture, codex)


class UsageValidationTest(unittest.TestCase):
    def sample(self):
        return [{'id': 'request-1', 'actual_usage': {'input_tokens': 1, 'output_tokens': 1}, 'estimated_usage': {'input_tokens': 20, 'output_tokens': 10}, 'usage': [{'source': 'provider_reported', 'input_tokens': 1, 'output_tokens': 1}, {'source': 'estimated', 'input_tokens': 20, 'output_tokens': 10}]}], [{'capture_id': 'request-1'}]

    def test_fixture_usage(self):
        rows, records = self.sample()
        validate_usage(rows, records, False, False)

    def test_duplicate_attempt_is_rejected(self):
        rows, records = self.sample()
        with self.assertRaises(RuntimeError):
            validate_usage(rows + rows, records, False, False)

    def test_zero_estimate_is_rejected(self):
        rows, records = self.sample()
        rows[0]['estimated_usage']['input_tokens'] = 0
        rows[0]['usage'][1]['input_tokens'] = 0
        with self.assertRaises(RuntimeError):
            validate_usage(rows, records, False, False)

    def test_wrong_actual_usage_is_rejected(self):
        rows, records = self.sample()
        rows[0]['actual_usage']['output_tokens'] = 2
        rows[0]['usage'][0]['output_tokens'] = 2
        with self.assertRaises(RuntimeError):
            validate_usage(rows, records, False, False)


class PlaintextValidationTest(unittest.TestCase):
    def test_ciphertext_control(self):
        reject_plaintext(b'encrypted-fixture', ['secret-marker'])

    def test_raw_plaintext_is_rejected(self):
        with self.assertRaises(RuntimeError):
            reject_plaintext(b'prefix secret-marker suffix', ['secret-marker'])

    def test_hex_plaintext_in_database_dump_is_rejected(self):
        with self.assertRaises(RuntimeError):
            reject_plaintext(b'\\x' + b'secret-marker'.hex().encode(), ['secret-marker'])

    def test_empty_markers_are_rejected(self):
        with self.assertRaises(RuntimeError):
            reject_plaintext(b'encrypted-fixture', [])


if __name__ == '__main__':
    unittest.main()
