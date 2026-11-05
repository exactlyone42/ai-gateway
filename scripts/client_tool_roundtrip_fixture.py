import json
import shlex


def handler(base):
    class ToolHandler(base):
        tool_mode = False
        tool_requests = 0
        tool_result_received = False
        tool_call_id = 'call_fixture_read'
        context_marker = ''
        expected_value = ''
        nonce_path = ''
        tool_name = ''
        original_user_messages = []
        context_preserved = False
        tool_rejection = ''

        def do_POST(self):
            cls = type(self)
            if not cls.tool_mode or self.path.startswith('/v1/messages/count_tokens'):
                return super().do_POST()
            self._record_path()
            body = self._json_body()
            codex = self.path.startswith('/v1/responses')
            self._capture('codex-cli' if codex else 'claude-code', body)
            cls.tool_requests += 1
            serialized = json.dumps(body)
            messages = body.get('input' if codex else 'messages', [])
            users = [message for message in messages if isinstance(message, dict) and message.get('role') == 'user']
            if cls.tool_requests == 1:
                cls.original_user_messages = [message for message in users if cls.context_marker in json.dumps(message)]
                if not cls.original_user_messages:
                    cls.tool_rejection = 'missing_original_user_message'
                    return self._send(422, 'application/json', b'{"error":"missing_original_user_message"}')
                if cls.expected_value in serialized:
                    return self._send(422, 'application/json', b'{"error":"tool_result_present_before_tool"}')
                if codex:
                    available = {tool.get('name'): tool for tool in body.get('tools', []) if tool.get('type') == 'function'}
                    cls.tool_name = next((name for name in ('exec_command', 'shell_command') if name in available), '')
                    if not cls.tool_name:
                        return self._send(422, 'application/json', b'{"error":"missing_read_tool"}')
                    field = 'cmd' if cls.tool_name == 'exec_command' else 'command'
                    arguments = json.dumps({field: 'cat -- ' + shlex.quote(cls.nonce_path)})
                    events = [json.loads(part[6:]) for part in (self.fixture_dir / 'openai-response.sse').read_text().split('\n\n') if part.startswith('data: ')]
                    item = {'id': 'fc_fixture_read', 'type': 'function_call', 'call_id': cls.tool_call_id, 'name': cls.tool_name, 'arguments': arguments, 'status': 'completed'}
                    added = dict(item, arguments='', status='in_progress')
                    response = dict(events[-1]['response'], output=[item])
                    events = [events[0], {'type': 'response.output_item.added', 'item': added, 'output_index': 0}, {'type': 'response.function_call_arguments.delta', 'item_id': item['id'], 'output_index': 0, 'delta': arguments}, {'type': 'response.function_call_arguments.done', 'item_id': item['id'], 'output_index': 0, 'arguments': arguments}, {'type': 'response.output_item.done', 'item': item, 'output_index': 0}, {'type': 'response.completed', 'response': response}]
                    payload = ''.join('data: ' + json.dumps(dict(event, sequence_number=index)) + '\n\n' for index, event in enumerate(events))
                else:
                    cls.tool_name = 'Read'
                    if not any(tool.get('name') == cls.tool_name for tool in body.get('tools', [])):
                        return self._send(422, 'application/json', b'{"error":"missing_read_tool"}')
                    events = [{'type': 'message_start', 'message': {'id': 'msg_fixture_read', 'type': 'message', 'role': 'assistant', 'content': [], 'model': body['model'], 'stop_reason': None, 'stop_sequence': None, 'usage': {'input_tokens': 1, 'output_tokens': 0}}}, {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'tool_use', 'id': cls.tool_call_id, 'name': 'Read', 'input': {}}}, {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'input_json_delta', 'partial_json': json.dumps({'file_path': cls.nonce_path})}}, {'type': 'content_block_stop', 'index': 0}, {'type': 'message_delta', 'delta': {'stop_reason': 'tool_use', 'stop_sequence': None}, 'usage': {'output_tokens': 1}}, {'type': 'message_stop'}]
                    payload = ''.join('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n' for event in events)
                return self._send(200, 'text/event-stream', payload.encode())
            if cls.tool_requests != 2:
                return self._send(422, 'application/json', b'{"error":"unexpected_tool_round"}')
            if not cls.original_user_messages or not all(original in users for original in cls.original_user_messages):
                cls.tool_rejection = 'missing_original_user_message'
                return self._send(422, 'application/json', b'{"error":"missing_original_user_message"}')
            cls.context_preserved = True
            if codex:
                blocks = body.get('input', [])
                outputs = [block for block in blocks if isinstance(block, dict) and block.get('type') == 'function_call_output' and block.get('call_id') == cls.tool_call_id]
                calls = [block for block in blocks if isinstance(block, dict) and block.get('type') == 'function_call' and block.get('call_id') == cls.tool_call_id and block.get('name') == cls.tool_name]
            else:
                blocks = [block for message in body.get('messages', []) for block in message.get('content', []) if isinstance(block, dict)]
                outputs = [block for block in blocks if block.get('type') == 'tool_result' and block.get('tool_use_id') == cls.tool_call_id and not block.get('is_error')]
                calls = [block for block in blocks if block.get('type') == 'tool_use' and block.get('id') == cls.tool_call_id and block.get('name') == cls.tool_name]
            if not calls or not any(cls.expected_value in json.dumps(output) for output in outputs):
                cls.tool_rejection = 'missing_real_tool_result_or_call'
                return self._send(422, 'application/json', b'{"error":"missing_real_tool_result_or_call"}')
            cls.tool_result_received = True
            name = 'openai-response.sse' if codex else 'anthropic-response.sse'
            return self._send(200, 'text/event-stream', (self.fixture_dir / name).read_bytes())

    return ToolHandler
