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

        def do_POST(self):
            if not self.path.startswith('/v1/responses'):
                return super().do_POST()
            self._record_path()
            body = self._json_body()
            self._capture('codex-cli', body)
            cls = type(self)
            content = json.dumps(body.get('input', []))
            compact = cls.compact_prompt in content
            if compact:
                cls.compact_requests += 1
                if cls.failure:
                    code = 'context_length_exceeded' if cls.failure == 'context-overflow' else 'invalid_request_error'
                    payload = {'error': {'message': cls.failure_marker, 'type': 'invalid_request_error', 'code': code}}
                    return self._send(400, 'application/json', json.dumps(payload).encode())
                text = 'SUMMARY_REMOVED_CONTROL' if cls.omit_summary else cls.summary
            else:
                if cls.compact_requests and cls.summary in content:
                    cls.resumed_requests += 1
                text = 'W06_CODEX_OK'
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
