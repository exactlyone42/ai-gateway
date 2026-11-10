import json


def handler(base):
    class CompactHandler(base):
        compact_prompt = 'GATEWAY_COMPACT_REQUEST'
        summary = 'GATEWAY_COMPACT_SUMMARY_KEEP_GOAL'
        compact_requests = 0
        resumed_requests = 0
        omit_summary = False
        failure = None
        failure_marker = 'GATEWAY_SUMMARY_FAILED'
        auto_claude = False
        normal_requests = 0
        overflow_requests = 0

        def do_POST(self):
            if not self.path.startswith(('/v1/responses', '/v1/messages')) or self.path.startswith('/v1/messages/count_tokens'):
                return super().do_POST()
            self._record_path()
            body = self._json_body()
            codex = self.path.startswith('/v1/responses')
            self._capture('codex-cli' if codex else 'claude-code', body)
            cls = type(self)
            messages = body.get('input' if codex else 'messages', [])
            content = json.dumps(messages)
            users = [message for message in messages if isinstance(message, dict) and message.get('role') == 'user']
            latest = users[-1].get('content', []) if users else []
            texts = [block.get('text', '') for block in latest if isinstance(block, dict) and block.get('type') == 'text'] if isinstance(latest, list) else [latest]
            instruction = '\n'.join(text for text in texts if not text.startswith('<system-reminder>'))
            compact = cls.compact_prompt in content if codex else 'Your task is to create a detailed summary of the conversation so far' in instruction
            if not codex:
                (self.capture_dir / ('compact-probe-' + str(len(self.captured_requests)) + '.json')).write_text(json.dumps({'compact': compact, 'instruction': instruction[:1000]}, indent=2))
            if compact:
                cls.compact_requests += 1
                if cls.failure:
                    code = 'context_length_exceeded' if cls.failure == 'context-overflow' else 'invalid_request_error'
                    payload = {'error': {'message': cls.failure_marker, 'type': 'invalid_request_error', 'code': code}}
                    return self._send(400, 'application/json', json.dumps(payload).encode())
                text = 'SUMMARY_REMOVED_CONTROL' if cls.omit_summary else cls.summary
                if not codex:
                    text = '<analysis>Preserve the reply goal.</analysis><summary>' + text + '</summary>'
            else:
                cls.normal_requests += 1
                if not codex and cls.auto_claude and cls.normal_requests == 2:
                    cls.overflow_requests += 1
                    payload = {'type': 'error', 'error': {'type': 'invalid_request_error', 'message': 'prompt is too long: 250000 tokens > 200000 maximum'}}
                    return self._send(400, 'application/json', json.dumps(payload).encode())
                if cls.compact_requests and cls.summary in content:
                    cls.resumed_requests += 1
                text = 'W06_CODEX_OK' if codex else 'W05_CLAUDE_CODE_OK'
                if not codex and cls.auto_claude and cls.normal_requests == 1:
                    events = [
                        {'type': 'message_start', 'message': {'id': 'msg_compact_read', 'type': 'message', 'role': 'assistant', 'content': [], 'model': body['model'], 'stop_reason': None, 'stop_sequence': None, 'usage': {'input_tokens': 25000, 'output_tokens': 0}}},
                        {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'tool_use', 'id': 'compact_read', 'name': 'Read', 'input': {}}},
                        {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'input_json_delta', 'partial_json': json.dumps({'file_path': cls.compact_file})}},
                        {'type': 'content_block_stop', 'index': 0},
                        {'type': 'message_delta', 'delta': {'stop_reason': 'tool_use', 'stop_sequence': None}, 'usage': {'output_tokens': 1}},
                        {'type': 'message_stop'},
                    ]
                    payload = ''.join('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n' for event in events)
                    return self._send(200, 'text/event-stream', payload.encode())
            if not codex:
                payload = (self.fixture_dir / 'anthropic-response.sse').read_text().replace('W05_CLAUDE_CODE_OK', text)
                payload = payload.replace('\"input_tokens\":1', '\"input_tokens\":' + ('100' if compact else '25000'))
                return self._send(200, 'text/event-stream', payload.encode())
            payload = (self.fixture_dir / 'openai-response.sse').read_text().replace('W06_CODEX_OK', text)
            events = []
            for part in payload.split('\n\n'):
                if part.startswith('data: '):
                    event = json.loads(part[6:])
                    if event.get('type') == 'response.completed':
                        usage = event['response']['usage']
                        usage['input_tokens'] = 100 if compact else 25000
                        usage['total_tokens'] = usage['input_tokens'] + usage['output_tokens']
                    events.append('data: ' + json.dumps(event) + '\n\n')
            return self._send(200, 'text/event-stream', ''.join(events).encode())

    return CompactHandler
