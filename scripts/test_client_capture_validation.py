import copy
import json
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from scripts.client_local_cancel import slots

from scripts.client_capture_validation import validate_tools, validate_usage, reject_plaintext, validate_errors, validate_owned_usage, validate_owned_tools, validate_owned_followup, validate_owned_compact, validate_passthrough_stream, validate_error_recovery, validate_long_stream, validate_owned_cancel
from pathlib import Path


class OwnedCancelTest(unittest.TestCase):
    def test_idle_runner_empty_token_list_is_supported(self):
        for token in ([], {'n_decoded': 8}, [{'n_decoded': 8}]):
            row = {'id': 0, 'id_task': 23, 'is_processing': bool(token), 'next_token': token}
            with patch('scripts.client_local_cancel.request_json', return_value=[row]):
                self.assertEqual(slots({'port': 1})['n_decoded'], 8 if token else 0)

    def sample(self, codex):
        args = StreamFailureRecoveryTest().sample(codex)
        content = args[0][0]
        content['client_response'] = content['provider_response']
        content['client_request'] = json.dumps({'model': 'public', 'input': 'cancel-marker'})
        content['provider_request'] = json.dumps({'model': 'qwen', 'input': 'cancel-marker'})
        args[3][0]['status'] = 'cancelled'
        args[3][0].update({'model': 'qwen', 'provider': 'local-ollama'})
        proof = {'during': {'is_processing': True, 'n_decoded': 8}, 'after': {'is_processing': False}, 'stop_seconds': 0.2}
        return [[content], args[3][:1], args[4][:1], 'qwen', 'cancel-marker', proof, codex]

    def test_real_partial_usage_and_stopped_inference(self):
        for codex in (False, True):
            validate_owned_cancel(*self.sample(codex))

    def test_backend_must_have_started_and_stopped(self):
        for field in ('during', 'after'):
            args = self.sample(True)
            args[5][field]['is_processing'] = field == 'after'
            with self.assertRaises(RuntimeError):
                validate_owned_cancel(*args)

    def test_partial_text_and_no_terminal_are_required(self):
        for codex in (False, True):
            args = self.sample(codex)
            args[0][0]['provider_response'] = args[0][0]['provider_response'].split('\n\n')[0] + '\n\n'
            args[0][0]['client_response'] = args[0][0]['provider_response']
            with self.assertRaises(RuntimeError):
                validate_owned_cancel(*args)

    def test_wrong_model_and_missing_input_rejected(self):
        for index, value in ((3, 'wrong-model'), (4, 'missing-marker')):
            args = self.sample(True)
            args[index] = value
            with self.assertRaises(RuntimeError):
                validate_owned_cancel(*args)

    def test_completed_accounting_is_not_cancelled(self):
        args = self.sample(True)
        args[1][0]['status'] = 'completed'
        with self.assertRaises(RuntimeError):
            validate_owned_cancel(*args)

    def test_reported_usage_must_not_be_lost_or_invented(self):
        for codex in (False, True):
            args = self.sample(codex)
            if codex:
                args[1][0]['usage'].append({'source': 'provider_reported', 'input_tokens': 2, 'output_tokens': 1})
            else:
                args[1][0]['usage'].pop()
            with self.assertRaises(RuntimeError):
                validate_owned_cancel(*args)


class LongStreamTest(unittest.TestCase):
    def sample(self, codex):
        name = 'openai-response.sse' if codex else 'anthropic-response.sse'
        stream = (Path(__file__).resolve().parent.parent / 'tests/fixtures/client-roundtrip' / name).read_text()
        return [[{'provider_response': stream}], stream, 660, 'W06_CODEX_OK' if codex else 'W05_CLAUDE_CODE_OK', codex]

    def test_complete_stream_beyond_real_timeout(self):
        for codex in (False, True):
            validate_long_stream(*self.sample(codex))

    def test_short_complete_reply_cannot_be_long_stream(self):
        args = self.sample(True)
        args[2] = 599
        with self.assertRaises(RuntimeError):
            validate_long_stream(*args)

    def test_delayed_partial_reply_cannot_be_complete(self):
        for codex in (False, True):
            args = self.sample(codex)
            stream = '\n\n'.join(args[1].split('\n\n')[:4 if codex else 3]) + '\n\n'
            args[0][0]['provider_response'] = stream
            args[1] = stream
            with self.assertRaises(RuntimeError):
                validate_long_stream(*args)

    def test_wrong_text_cannot_finish(self):
        args = self.sample(True)
        args[3] = 'wrong'
        with self.assertRaises(RuntimeError):
            validate_long_stream(*args)

    def test_multiple_completions_cannot_pass(self):
        args = self.sample(True)
        args[1] += args[1]
        args[0][0]['provider_response'] = args[1]
        with self.assertRaises(RuntimeError):
            validate_long_stream(*args)


class ErrorRecoveryTest(unittest.TestCase):
    def sample(self, codex):
        name = 'openai-response.sse' if codex else 'anthropic-response.sse'
        stream = (Path(__file__).resolve().parent.parent / 'tests/fixtures/client-roundtrip' / name).read_text()
        payload = json.dumps({'error': {'type': 'authentication_error'}, 'request_id': 'provider-error-1'})
        saved = [{'provider_response': payload}, {'provider_response': stream}]
        records = [{'capture_id': 'failed-1'}, {'capture_id': 'completed-2'}]
        estimated = {'source': 'estimated', 'input_tokens': 20, 'output_tokens': 10}
        rows = [
            {'id': 'failed-1', 'provider_request_id': 'provider-error-1', 'status': 'failed', 'actual_usage': {}, 'estimated_usage': {'input_tokens': 20, 'output_tokens': 10}, 'usage': [estimated]},
            {'id': 'completed-2', 'status': 'completed', 'actual_usage': {'input_tokens': 1, 'output_tokens': 1}, 'estimated_usage': {'input_tokens': 20, 'output_tokens': 10}, 'usage': [estimated, {'source': 'provider_reported', 'input_tokens': 1, 'output_tokens': 1}]},
        ]
        return [saved, [payload], stream, rows, records, codex]

    def test_error_and_separate_success_are_required(self):
        for codex in (False, True):
            validate_error_recovery(*self.sample(codex))

    def test_missing_or_reversed_error_is_rejected(self):
        for codex in (False, True):
            args = self.sample(codex)
            args[0].reverse()
            with self.assertRaises(RuntimeError):
                validate_error_recovery(*args)
            args = self.sample(codex)
            args[0].pop(0)
            with self.assertRaises(RuntimeError):
                validate_error_recovery(*args)

    def test_truncated_recovery_stream_is_rejected(self):
        for codex in (False, True):
            args = self.sample(codex)
            args[0][1]['provider_response'] = args[2][:-10]
            with self.assertRaises(RuntimeError):
                validate_error_recovery(*args)

    def test_duplicate_capture_id_is_rejected(self):
        args = self.sample(True)
        args[4][1]['capture_id'] = args[4][0]['capture_id']
        with self.assertRaises(RuntimeError):
            validate_error_recovery(*args)

    def test_rejected_request_cannot_report_usage(self):
        args = self.sample(True)
        args[3][0]['usage'].append({'source': 'provider_reported', 'input_tokens': 1, 'output_tokens': 1})
        with self.assertRaises(RuntimeError):
            validate_error_recovery(*args)

    def test_failed_recovery_cannot_be_completed(self):
        args = self.sample(True)
        args[3][1]['status'] = 'failed'
        with self.assertRaises(RuntimeError):
            validate_error_recovery(*args)

    def test_missing_recovery_usage_is_rejected(self):
        args = self.sample(True)
        args[3][1]['usage'].pop()
        with self.assertRaises(RuntimeError):
            validate_error_recovery(*args)

    def test_unmatched_provider_request_id_is_rejected(self):
        args = self.sample(True)
        args[3][0]['provider_request_id'] = 'different-error'
        with self.assertRaises(RuntimeError):
            validate_error_recovery(*args)


class StreamFailureRecoveryTest(unittest.TestCase):
    def sample(self, codex):
        args = ErrorRecoveryTest().sample(codex)
        parts = args[2].split('\n\n')
        opening = '\n\n'.join(parts[:4 if codex else 3]) + '\n\n'
        args[0][0]['provider_response'] = opening
        args[1] = [opening]
        failed = args[3][0]
        failed['provider_request_id'] = 'fixture-error-1'
        failed['error_type'] = 'upstream_body_error'
        if not codex:
            failed['actual_usage'] = {'input_tokens': 1, 'output_tokens': 0}
            failed['usage'].append({'source': 'provider_reported', 'input_tokens': 1, 'output_tokens': 0})
        args[4][0]['state'] = 'incomplete'
        args[4][1]['state'] = 'complete'
        return args

    def validate(self, args):
        validate_error_recovery(*args, interrupted=True, upstream_request_id='fixture-error-1')

    def test_partial_stream_and_new_request_complete(self):
        for codex in (False, True):
            self.validate(self.sample(codex))

    def test_headers_alone_are_not_started_generation(self):
        for codex in (False, True):
            args = self.sample(codex)
            opening = args[1][0].split('\n\n')[0] + '\n\n'
            args[0][0]['provider_response'] = opening
            args[1] = [opening]
            with self.assertRaises(RuntimeError):
                self.validate(args)

    def test_completed_stream_is_not_an_interruption(self):
        for codex in (False, True):
            args = self.sample(codex)
            args[0][0]['provider_response'] = args[2]
            args[1] = [args[2]]
            with self.assertRaises(RuntimeError):
                self.validate(args)

    def test_interruption_must_leave_incomplete_capture(self):
        args = self.sample(True)
        args[4][0]['state'] = 'complete'
        with self.assertRaises(RuntimeError):
            self.validate(args)

    def test_failure_reason_cannot_be_missing(self):
        args = self.sample(True)
        args[3][0]['error_type'] = None
        with self.assertRaises(RuntimeError):
            self.validate(args)

    def test_started_failure_cannot_be_cancelled(self):
        args = self.sample(True)
        args[3][0]['status'] = 'cancelled'
        with self.assertRaises(RuntimeError):
            self.validate(args)

    def test_known_partial_usage_cannot_disappear(self):
        args = self.sample(False)
        args[3][0]['usage'].pop()
        with self.assertRaises(RuntimeError):
            self.validate(args)

    def test_unknown_usage_cannot_be_invented(self):
        args = self.sample(True)
        args[3][0]['usage'].append({'source': 'provider_reported', 'input_tokens': 1, 'output_tokens': 0})
        with self.assertRaises(RuntimeError):
            self.validate(args)


class PassthroughStreamTest(unittest.TestCase):
    stream = 'event: message_start\ndata: {"text":"keepalive"}\n\nevent: message_stop\ndata: {}\n\n'

    def test_only_keepalive_frames_are_ignored(self):
        client = ': keepalive\n\n' + self.stream.replace('event: message_stop', ': keepalive\n\nevent: message_stop') + ': keepalive\n\n'
        validate_passthrough_stream(self.stream, client)
        validate_passthrough_stream(client, self.stream)

    def test_changed_data_is_rejected(self):
        with self.assertRaises(RuntimeError):
            validate_passthrough_stream(self.stream, self.stream.replace('keepalive', 'changed'))

    def test_missing_terminal_event_is_rejected(self):
        with self.assertRaises(RuntimeError):
            validate_passthrough_stream(self.stream, self.stream.split('event: message_stop')[0])

    def test_other_comments_are_preserved(self):
        with self.assertRaises(RuntimeError):
            validate_passthrough_stream(self.stream, ': other\n\n' + self.stream)

    def test_keepalive_text_inside_data_is_preserved(self):
        stream = 'data: first\n: keepalive\n\n'
        with self.assertRaises(RuntimeError):
            validate_passthrough_stream(stream, 'data: first\n\n')

    def test_missing_final_separator_is_rejected(self):
        with self.assertRaises(RuntimeError):
            validate_passthrough_stream(self.stream, self.stream[:-2])


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

    def test_error_has_no_reported_usage(self):
        rows, records = self.sample()
        rows[0]['status'] = 'failed'
        rows[0]['actual_usage'] = {}
        rows[0]['usage'].pop(0)
        validate_usage(rows, records, False, False, True)
        rows[0]['usage'].append({'source': 'provider_reported', 'input_tokens': 1, 'output_tokens': 1})
        with self.assertRaises(RuntimeError):
            validate_usage(rows, records, False, False, True)

    def test_error_cannot_be_recorded_as_success(self):
        rows, records = self.sample()
        rows[0]['status'] = 'completed'
        with self.assertRaises(RuntimeError):
            validate_usage(rows, records, False, False, True)

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


class ErrorValidationTest(unittest.TestCase):
    def test_each_unique_error_is_saved(self):
        validate_errors([{'provider_response': 'error-2'}, {'provider_response': 'error-1'}], ['error-1', 'error-2'])

    def test_duplicate_cannot_replace_an_unsaved_error(self):
        with self.assertRaises(RuntimeError):
            validate_errors([{'provider_response': 'error-1'}, {'provider_response': 'error-1'}], ['error-1', 'error-2'])

    def test_sources_must_be_distinguishable(self):
        with self.assertRaises(RuntimeError):
            validate_errors([{'provider_response': 'error-1'}, {'provider_response': 'error-1'}], ['error-1', 'error-1'])


if __name__ == '__main__':
    unittest.main()


class OwnedUsageValidationTest(unittest.TestCase):
    def native_sample(self):
        saved, rows, records = self.sample()
        events = [
            {'type': 'message_start', 'message': {'usage': {'input_tokens': 7, 'output_tokens': 0}}},
            {'type': 'content_block_delta', 'delta': {'type': 'text_delta', 'text': 'OWNED_OK'}},
            {'type': 'message_delta', 'usage': {'output_tokens': 2}},
            {'type': 'message_stop'},
        ]
        saved[0]['provider_response'] = ''.join('data: ' + json.dumps(event) + '\n\n' for event in events)
        return saved, rows, records

    def test_native_claude_usage_requires_complete_stream(self):
        saved, rows, records = self.native_sample()
        validate_owned_usage(saved, rows, records, 'OWNED_OK', False)
        saved[0]['provider_response'] = saved[0]['provider_response'].replace('message_stop', 'message_pending')
        with self.assertRaisesRegex(RuntimeError, '完整完成'):
            validate_owned_usage(saved, rows, records, 'OWNED_OK', False)

    def test_native_usage_cannot_be_replaced_by_matching_ledger_fields(self):
        saved, rows, records = self.native_sample()
        for value in (rows[0]['actual_usage'], rows[0]['usage'][0]):
            value['input_tokens'] = 9
        with self.assertRaisesRegex(RuntimeError, '供應商回應'):
            validate_owned_usage(saved, rows, records, 'OWNED_OK', False)

    def sample(self):
        event = {'type': 'response.completed', 'response': {'output': [{'text': 'OWNED_OK'}], 'usage': {'input_tokens': 7, 'output_tokens': 2}}}
        saved = [{'provider_response': 'data: ' + json.dumps({'type': 'response.output_text.done', 'text': 'OWNED_OK'}) + '\n\ndata: ' + json.dumps(event) + '\n\n'}]
        rows = [{'id': 'capture-1', 'status': 'completed', 'actual_usage': {'input_tokens': 7, 'output_tokens': 2}, 'estimated_usage': {'input_tokens': 5, 'output_tokens': 3}, 'usage': [{'source': 'provider_reported', 'input_tokens': 7, 'output_tokens': 2}, {'source': 'estimated', 'input_tokens': 5, 'output_tokens': 3}]}]
        for value in (rows[0]['actual_usage'], rows[0]['usage'][0]):
            value.update({'cached_input_tokens': 0, 'cache_write_input_tokens': 0, 'reasoning_tokens': 0})
        return saved, rows, [{'capture_id': 'capture-1'}]

    def test_provider_completion_matches_ledger(self):
        validate_owned_usage(*self.sample(), 'OWNED_OK')

    def test_matching_ledger_fields_cannot_replace_provider_usage(self):
        saved, rows, records = self.sample()
        rows[0]['actual_usage']['output_tokens'] = 8
        rows[0]['usage'][0]['output_tokens'] = 8
        with self.assertRaisesRegex(RuntimeError, '供應商回應'):
            validate_owned_usage(saved, rows, records, 'OWNED_OK')

    def test_truncated_provider_stream_is_rejected(self):
        saved, rows, records = self.sample()
        saved[0]['provider_response'] = 'data: {"type":"response.created"}\n\n'
        with self.assertRaisesRegex(RuntimeError, '完整完成'):
            validate_owned_usage(saved, rows, records, 'OWNED_OK')


    def test_sse_optional_space_is_supported(self):
        saved, rows, records = self.sample()
        saved[0]['provider_response'] = saved[0]['provider_response'].replace('data: ', 'data:')
        validate_owned_usage(saved, rows, records, 'OWNED_OK')


    def test_completed_event_need_not_repeat_final_text(self):
        saved, rows, records = self.sample()
        events = [json.loads(line[6:]) for line in saved[0]['provider_response'].splitlines() if line.startswith('data: ')]
        events[-1]['response']['output'] = []
        saved[0]['provider_response'] = ''.join('data: ' + json.dumps(event) + '\n\n' for event in events)
        validate_owned_usage(saved, rows, records, 'OWNED_OK')

    def test_wrong_final_text_is_rejected(self):
        saved, rows, records = self.sample()
        saved[0]['provider_response'] = saved[0]['provider_response'].replace('OWNED_OK', 'WRONG')
        with self.assertRaisesRegex(RuntimeError, '完整完成'):
            validate_owned_usage(saved, rows, records, 'OWNED_OK')


    def test_cache_and_reasoning_are_counted_once(self):
        saved, rows, records = self.sample()
        events = [json.loads(line[6:]) for line in saved[0]['provider_response'].splitlines() if line.startswith('data: ')]
        events[-1]['response']['usage'].update({'input_tokens_details': {'cached_tokens': 2}, 'output_tokens_details': {'reasoning_tokens': 1}})
        saved[0]['provider_response'] = ''.join('data: ' + json.dumps(event) + '\n\n' for event in events)
        for value in (rows[0]['actual_usage'], rows[0]['usage'][0]):
            value.update({'input_tokens': 5, 'output_tokens': 1, 'cached_input_tokens': 2, 'reasoning_tokens': 1})
        validate_owned_usage(saved, rows, records, 'OWNED_OK')
        for value in (rows[0]['actual_usage'], rows[0]['usage'][0]):
            value['input_tokens'] = 7
        with self.assertRaisesRegex(RuntimeError, '供應商回應'):
            validate_owned_usage(saved, rows, records, 'OWNED_OK')


class OwnedToolsValidationTest(unittest.TestCase):
    def native_sample(self):
        original = {'role': 'user', 'content': 'Read the isolated file.'}
        event = {'type': 'content_block_start', 'content_block': {'type': 'tool_use', 'id': 'native-call', 'name': 'Read'}}
        first = {'provider_response': 'data: ' + json.dumps(event) + '\n\n'}
        result = {'role': 'user', 'content': [{'type': 'tool_result', 'tool_use_id': 'native-call', 'content': 'random-file-value'}]}
        second = {}
        for leg in ('client_request', 'provider_request'):
            first[leg] = json.dumps({'messages': [original]})
            second[leg] = json.dumps({'messages': [original, result]})
        return [first, second]

    def test_native_claude_tools_preserve_result_and_call(self):
        saved = self.native_sample()
        validate_owned_tools(saved, 'random-file-value', False)
        saved[1]['provider_request'] = saved[1]['provider_request'].replace('native-call', 'wrong-call')
        with self.assertRaisesRegex(RuntimeError, '未對應'):
            validate_owned_tools(saved, 'random-file-value', False)

    def test_native_tool_error_is_not_a_successful_result(self):
        saved = self.native_sample()
        body = json.loads(saved[1]['client_request'])
        body['messages'][1]['content'][0]['is_error'] = True
        saved[1]['client_request'] = json.dumps(body)
        with self.assertRaisesRegex(RuntimeError, '未對應'):
            validate_owned_tools(saved, 'random-file-value', False)

    def sample(self):
        original = {'role': 'user', 'content': 'Read the isolated file.'}
        event = {'type': 'response.output_item.done', 'item': {'type': 'function_call', 'call_id': 'owned-call', 'name': 'exec_command'}}
        first = {'provider_response': 'data: ' + json.dumps(event) + '\n\n'}
        second = {}
        for leg in ('client_request', 'provider_request'):
            first[leg] = json.dumps({'input': [original]})
            second[leg] = json.dumps({'input': [original, {'type': 'function_call_output', 'call_id': 'owned-call', 'output': 'random-file-value'}]})
        return [first, second]

    def test_real_result_and_original_message_are_preserved(self):
        validate_owned_tools(self.sample(), 'random-file-value')

    def test_wrong_tool_call_id_is_rejected(self):
        saved = self.sample()
        saved[1]['provider_request'] = saved[1]['provider_request'].replace('owned-call', 'different-call')
        with self.assertRaisesRegex(RuntimeError, '未對應'):
            validate_owned_tools(saved, 'random-file-value')

    def test_tool_value_cannot_replace_original_message(self):
        saved = self.sample()
        body = json.loads(saved[1]['client_request'])
        body['input'] = [body['input'][1]]
        saved[1]['client_request'] = json.dumps(body)
        with self.assertRaisesRegex(RuntimeError, '遺失原訊息'):
            validate_owned_tools(saved, 'random-file-value')


    def followup_sample(self, codex):
        saved = self.sample() if codex else self.native_sample()
        event = json.loads(saved[0]['provider_response'].splitlines()[0][5:])
        call = event['item' if codex else 'content_block']
        for leg in ('client_request', 'provider_request'):
            body = json.loads(saved[1][leg])
            body['input' if codex else 'messages'].insert(1, call if codex else {'role': 'assistant', 'content': [call]})
            saved[1][leg] = json.dumps(body)
        third = copy.deepcopy(saved[1])
        for leg in ('client_request', 'provider_request'):
            body = json.loads(third[leg])
            body['input' if codex else 'messages'].append({'role': 'user', 'content': [{'type': 'input_text' if codex else 'text', 'text': 'Use the previous result.'}]})
            third[leg] = json.dumps(body)
        third['provider_response'] = 'data: ' + json.dumps({'type': 'response.completed' if codex else 'message_stop'}) + '\n\n'
        return saved + [third]

    def test_followup_preserves_original_context_and_tool_output(self):
        for codex in (False, True):
            validate_owned_followup(self.followup_sample(codex), 'random-file-value', 'Use the previous result.', codex)

    def test_tool_roundtrip_alone_does_not_prove_user_followup(self):
        with self.assertRaisesRegex(RuntimeError, '三次請求'):
            validate_owned_followup(self.sample(), 'random-file-value', 'Use the previous result.')

    def test_claude_may_move_the_cache_hint_without_changing_tool_content(self):
        saved = self.followup_sample(False)
        for leg in ('client_request', 'provider_request'):
            body = json.loads(saved[1][leg])
            body['messages'][2]['content'][0]['cache_control'] = {'type': 'ephemeral'}
            saved[1][leg] = json.dumps(body)
        validate_owned_followup(saved, 'random-file-value', 'Use the previous result.', False)

    def test_followup_missing_tool_output_is_rejected(self):
        for codex in (False, True):
            for leg in ('client_request', 'provider_request'):
                saved = self.followup_sample(codex)
                body = json.loads(saved[2][leg])
                if codex:
                    body['input'] = [item for item in body['input'] if item.get('type') != 'function_call_output']
                else:
                    for item in body['messages']:
                        if isinstance(item.get('content'), list):
                            item['content'] = [block for block in item['content'] if block.get('type') != 'tool_result']
                saved[2][leg] = json.dumps(body)
                with self.assertRaisesRegex(RuntimeError, '未對應'):
                    validate_owned_followup(saved, 'random-file-value', 'Use the previous result.', codex)

    def test_followup_changed_tool_id_is_rejected(self):
        for codex in (False, True):
            for leg in ('client_request', 'provider_request'):
                saved = self.followup_sample(codex)
                saved[2][leg] = saved[2][leg].replace('owned-call' if codex else 'native-call', 'different-call')
                with self.assertRaisesRegex(RuntimeError, '未對應'):
                    validate_owned_followup(saved, 'random-file-value', 'Use the previous result.', codex)

    def test_followup_keeps_more_than_the_random_marker(self):
        for codex in (False, True):
            for leg in ('client_request', 'provider_request'):
                saved = self.followup_sample(codex)
                saved[1][leg] = saved[1][leg].replace('random-file-value', 'random-file-value full-tool-context')
                with self.assertRaisesRegex(RuntimeError, '未完整保留'):
                    validate_owned_followup(saved, 'random-file-value', 'Use the previous result.', codex)

    def test_followup_cannot_drop_original_user_message(self):
        for codex in (False, True):
            for leg in ('client_request', 'provider_request'):
                saved = self.followup_sample(codex)
                body = json.loads(saved[2][leg])
                body['input' if codex else 'messages'].pop(0)
                saved[2][leg] = json.dumps(body)
                with self.assertRaisesRegex(RuntimeError, '遺失原訊息'):
                    validate_owned_followup(saved, 'random-file-value', 'Use the previous result.', codex)

    def test_assistant_text_is_not_a_second_user_instruction(self):
        for codex in (False, True):
            for leg in ('client_request', 'provider_request'):
                saved = self.followup_sample(codex)
                body = json.loads(saved[2][leg])
                body['input' if codex else 'messages'][-1]['role'] = 'assistant'
                saved[2][leg] = json.dumps(body)
                with self.assertRaisesRegex(RuntimeError, '第二輪使用者指令'):
                    validate_owned_followup(saved, 'random-file-value', 'Use the previous result.', codex)

    def test_followup_cannot_read_the_file_again(self):
        for codex in (False, True):
            saved = self.followup_sample(codex)
            saved[2]['provider_response'] = saved[0]['provider_response']
            with self.assertRaisesRegex(RuntimeError, '重新呼叫工具'):
                validate_owned_followup(saved, 'random-file-value', 'Use the previous result.', codex)

    def test_followup_cannot_drop_the_original_tool_call(self):
        for codex in (False, True):
            for leg in ('client_request', 'provider_request'):
                saved = self.followup_sample(codex)
                body = json.loads(saved[2][leg])
                if codex:
                    body['input'] = [item for item in body['input'] if item.get('type') != 'function_call']
                else:
                    for item in body['messages']:
                        if isinstance(item.get('content'), list):
                            item['content'] = [block for block in item['content'] if block.get('type') != 'tool_use']
                saved[2][leg] = json.dumps(body)
                with self.assertRaisesRegex(RuntimeError, '原工具要求'):
                    validate_owned_followup(saved, 'random-file-value', 'Use the previous result.', codex)

    def test_followup_cannot_change_the_original_tool_name(self):
        for codex in (False, True):
            for leg in ('client_request', 'provider_request'):
                saved = self.followup_sample(codex)
                body = json.loads(saved[2][leg])
                call = body['input'][1] if codex else body['messages'][1]['content'][0]
                call['name'] = 'different-tool'
                saved[2][leg] = json.dumps(body)
                with self.assertRaisesRegex(RuntimeError, '原工具要求'):
                    validate_owned_followup(saved, 'random-file-value', 'Use the previous result.', codex)


class OwnedCompactValidationTest(unittest.TestCase):
    def sample(self):
        fields = ['goal-a', 'decision-b', 'constraint-c', 'next-d']
        summary = 'Remember:\n' + '\n'.join(fields)
        saved = [{'provider_response': ''}, {'provider_response': 'data: ' + json.dumps({'type': 'response.output_text.done', 'text': summary})}, {'provider_response': ''}]
        for leg in ('client_request', 'provider_request'):
            for content, text in zip(saved, (' '.join(fields), 'GATEWAY_OWNED_COMPACT', summary)):
                content[leg] = json.dumps({'input': [{'role': 'user', 'content': text}]})
        return saved, fields

    def test_true_summary_and_continuation_preserve_four_fields(self):
        result = validate_owned_compact(*self.sample())
        self.assertEqual(result['preserved_fields'], 4)

    def test_summary_cannot_drop_any_field(self):
        for index in range(4):
            saved, fields = self.sample()
            saved[1]['provider_response'] = saved[1]['provider_response'].replace(fields[index], 'omitted')
            with self.assertRaisesRegex(RuntimeError, '遺失'):
                validate_owned_compact(saved, fields)

    def test_original_fields_cannot_replace_missing_summary(self):
        for leg in ('client_request', 'provider_request'):
            saved, fields = self.sample()
            saved[2][leg] = saved[0][leg]
            with self.assertRaisesRegex(RuntimeError, '未完整帶入'):
                validate_owned_compact(saved, fields)

    def test_missing_summary_instruction_is_rejected(self):
        saved, fields = self.sample()
        saved[1]['provider_request'] = saved[0]['provider_request']
        with self.assertRaisesRegex(RuntimeError, '摘要指令'):
            validate_owned_compact(saved, fields)

    def test_two_turns_cannot_replace_compaction(self):
        saved, fields = self.sample()
        with self.assertRaisesRegex(RuntimeError, '三輪'):
            validate_owned_compact(saved[:2], fields)
