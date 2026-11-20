import http.client
import json
import os
import stat
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

import claude_bridge

FAKE = '''#!/usr/bin/env python3
import json, sys
prompt = sys.stdin.read()
args = sys.argv[1:]
mode = 'stream' if 'stream-json' in args else 'json'
if 'FAIL' in prompt:
    print(json.dumps({'type': 'result', 'is_error': True, 'result': 'Claude AI usage limit reached'}))
    sys.exit(1)
if 'CRASH' in prompt:
    sys.stderr.write('boom')
    sys.exit(3)
usage = {'input_tokens': 2, 'cache_creation_input_tokens': 5, 'cache_read_input_tokens': 3, 'output_tokens': 4}
text = 'sys=' + args[args.index('--system-prompt') + 1] + '|prompt=' + prompt
if 'CALLTOOL' in prompt:
    text = 'ok <tool_calls>[{"name": "draw", "arguments": {"x": 1}}, {"name": "draw", "arguments": "{}"}]</tool_calls>'
if 'BADTOOL' in prompt:
    text = '<tool_calls>[{"name": "other", "arguments": {}}]</tool_calls>'
if mode == 'json':
    print(json.dumps({'type': 'result', 'is_error': False, 'result': text, 'usage': usage}))
else:
    for piece in (text[:5], text[5:]):
        print(json.dumps({'type': 'stream_event', 'event': {'type': 'content_block_delta', 'delta': {'type': 'text_delta', 'text': piece}}}), flush=True)
    print(json.dumps({'type': 'result', 'is_error': False, 'result': text, 'usage': usage}))
'''


class BuildPromptTests(unittest.TestCase):
    def test_system_messages_are_joined_and_single_turn_is_plain(self):
        system, prompt = claude_bridge.build_prompt({'messages': [
            {'role': 'system', 'content': 'a'}, {'role': 'developer', 'content': 'b'},
            {'role': 'user', 'content': [{'type': 'text', 'text': 'hi'}]}]})
        self.assertEqual((system, prompt), ('a\n\nb', 'hi'))

    def test_multi_turn_keeps_history_in_order(self):
        _, prompt = claude_bridge.build_prompt({'messages': [
            {'role': 'user', 'content': 'q1'}, {'role': 'assistant', 'content': 'a1'}, {'role': 'user', 'content': 'q2'}]})
        self.assertLess(prompt.index('q1'), prompt.index('a1'))
        self.assertLess(prompt.index('a1'), prompt.index('q2'))

    def test_unsupported_requests_are_rejected(self):
        user = {'role': 'user', 'content': 'x'}
        for body in [{'messages': [user], 'tools': [{'type': 'function'}]},
                     {'messages': [{'role': 'user', 'content': [{'type': 'image_url'}]}]},
                     {'messages': [{'role': 'assistant', 'content': 'x'}]},
                     {'messages': [{'role': 'function', 'content': 'x'}, user]}]:
            with self.assertRaises(claude_bridge.BridgeError) as context:
                claude_bridge.build_prompt(body)
            self.assertEqual(context.exception.status, 400)


TOOLS = [{'type': 'function', 'function': {'name': 'draw', 'description': 'draw', 'parameters': {'type': 'object'}}}]


class ToolPromptTests(unittest.TestCase):
    def test_tools_go_into_system_and_history_is_rendered(self):
        system, prompt = claude_bridge.build_prompt({'tools': TOOLS, 'tool_choice': 'required', 'messages': [
            {'role': 'user', 'content': 'q'},
            {'role': 'assistant', 'content': None, 'tool_calls': [{'id': 'c1', 'type': 'function', 'function': {'name': 'draw', 'arguments': '{"x": 1}'}}]},
            {'role': 'tool', 'tool_call_id': 'c1', 'content': 'done'}]})
        self.assertIn('"name": "draw"', system)
        self.assertIn('must call at least one tool', system)
        self.assertIn('<tool_calls>[{"name": "draw", "arguments": {"x": 1}}]</tool_calls>', prompt)
        self.assertIn('[tool result]\nname=draw id=c1\ndone', prompt)

    def test_tool_choice_none_hides_tools_and_trailing_assistant_is_rejected(self):
        system, _ = claude_bridge.build_prompt({'tools': TOOLS, 'tool_choice': 'none', 'messages': [{'role': 'user', 'content': 'q'}]})
        self.assertNotIn('draw', system)
        with self.assertRaises(claude_bridge.BridgeError):
            claude_bridge.build_prompt({'messages': [{'role': 'user', 'content': 'q'}, {'role': 'assistant', 'content': 'a'}]})

    def test_extract_tool_calls(self):
        text, calls = claude_bridge.extract_tool_calls('hi <tool_calls>[{"name": "draw", "arguments": {"x": 1}}]</tool_calls>', {'draw'})
        self.assertEqual((text, calls[0]['function']), ('hi', {'name': 'draw', 'arguments': '{"x": 1}'}))
        for raw in ['plain', '<tool_calls>not json</tool_calls>', '<tool_calls>[{"name": "x", "arguments": {}}]</tool_calls>', '<tool_calls>[]</tool_calls>']:
            self.assertEqual(claude_bridge.extract_tool_calls(raw, {'draw'}), (raw, []))


class ServerTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        fake = Path(directory.name) / 'claude'
        fake.write_text(FAKE)
        fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
        for name, value in [('CLAUDE_BIN', str(fake)), ('WORK_DIR', directory.name)]:
            patcher = patch.object(claude_bridge, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), claude_bridge.Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)

    def request(self, method, path, body=None):
        connection = http.client.HTTPConnection('127.0.0.1', self.server.server_address[1], timeout=20)
        connection.request(method, path, json.dumps(body) if body is not None else None, {'Content-Type': 'application/json'})
        response = connection.getresponse()
        return response.status, response.read().decode()

    def chat(self, **extra):
        return {'model': 'claude-sonnet-5-5', 'messages': [{'role': 'system', 'content': 'S'}, {'role': 'user', 'content': 'hello'}], **extra}

    def test_health_and_models(self):
        self.assertEqual(self.request('GET', '/api/version')[0], 200)
        status, text = self.request('GET', '/v1/models')
        self.assertEqual(status, 200)
        self.assertIn('claude-opus-5-5', text)

    def test_non_stream_returns_text_and_usage(self):
        status, text = self.request('POST', '/v1/chat/completions', self.chat())
        body = json.loads(text)
        self.assertEqual(status, 200)
        self.assertEqual(body['choices'][0]['message']['content'], 'sys=S|prompt=hello')
        self.assertEqual(body['usage'], {'prompt_tokens': 10, 'completion_tokens': 4, 'total_tokens': 14})

    def test_stream_emits_deltas_finish_usage_and_done(self):
        status, text = self.request('POST', '/v1/chat/completions', self.chat(stream=True))
        self.assertEqual(status, 200)
        events = [line[6:] for line in text.splitlines() if line.startswith('data: ')]
        self.assertEqual(events[-1], '[DONE]')
        chunks = [json.loads(item) for item in events[:-1]]
        self.assertEqual(''.join(c['choices'][0]['delta'].get('content', '') for c in chunks), 'sys=S|prompt=hello')
        self.assertEqual(chunks[-1]['choices'][0]['finish_reason'], 'stop')
        self.assertEqual(chunks[-1]['usage']['total_tokens'], 14)

    def test_usage_limit_maps_to_429_in_both_modes(self):
        body = {'model': 'claude-sonnet-5-5', 'messages': [{'role': 'user', 'content': 'FAIL'}]}
        self.assertEqual(self.request('POST', '/v1/chat/completions', body)[0], 429)
        self.assertEqual(self.request('POST', '/v1/chat/completions', {**body, 'stream': True})[0], 429)

    def test_crash_without_result_is_502_in_both_modes(self):
        body = {'model': 'claude-sonnet-5-5', 'messages': [{'role': 'user', 'content': 'CRASH'}]}
        self.assertEqual(self.request('POST', '/v1/chat/completions', body)[0], 502)
        self.assertEqual(self.request('POST', '/v1/chat/completions', {**body, 'stream': True})[0], 502)

    def test_tool_calls_non_stream(self):
        status, text = self.request('POST', '/v1/chat/completions', self.chat(tools=TOOLS, messages=[{'role': 'user', 'content': 'CALLTOOL'}]))
        choice = json.loads(text)['choices'][0]
        self.assertEqual((status, choice['finish_reason'], choice['message']['content']), (200, 'tool_calls', 'ok'))
        self.assertEqual([c['function'] for c in choice['message']['tool_calls']],
                         [{'name': 'draw', 'arguments': '{"x": 1}'}, {'name': 'draw', 'arguments': '{}'}])

    def test_tool_calls_stream(self):
        status, text = self.request('POST', '/v1/chat/completions', self.chat(tools=TOOLS, stream=True, messages=[{'role': 'user', 'content': 'CALLTOOL'}]))
        events = [line[6:] for line in text.splitlines() if line.startswith('data: ')]
        chunks = [json.loads(item) for item in events[:-1]]
        deltas = [c['choices'][0]['delta'] for c in chunks if c['choices'][0]['delta'].get('tool_calls')]
        self.assertEqual((status, events[-1]), (200, '[DONE]'))
        self.assertEqual([c['index'] for c in deltas[0]['tool_calls']], [0, 1])
        self.assertEqual(deltas[0]['tool_calls'][0]['function']['name'], 'draw')
        self.assertEqual(chunks[-1]['choices'][0]['finish_reason'], 'tool_calls')
        self.assertEqual(chunks[-1]['usage']['total_tokens'], 14)

    def test_unknown_tool_or_plain_answer_stays_text(self):
        for word in ('BADTOOL', 'hello'):
            body = json.loads(self.request('POST', '/v1/chat/completions', self.chat(tools=TOOLS, messages=[{'role': 'user', 'content': word}]))[1])
            self.assertEqual(body['choices'][0]['finish_reason'], 'stop')
            self.assertNotIn('tool_calls', body['choices'][0]['message'])

    def test_bad_model_tools_and_json_are_client_errors(self):
        self.assertEqual(self.request('POST', '/v1/chat/completions', self.chat(model='gpt-5'))[0], 404)
        self.assertEqual(self.request('POST', '/v1/chat/completions', self.chat(tools=[{'type': 'function'}]))[0], 400)
        connection = http.client.HTTPConnection('127.0.0.1', self.server.server_address[1], timeout=20)
        connection.request('POST', '/v1/chat/completions', 'not json', {'Content-Length': '8'})
        self.assertEqual(connection.getresponse().status, 400)


if __name__ == '__main__':
    unittest.main()
