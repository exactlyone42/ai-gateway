import hashlib
import json


def handler(base):
    class ErrorHandler(base):
        error_requests = 0
        error_observations = []
        error_marker = 'fixture-upstream-request-rejected'
        error_payloads = []
        success_control = False

        def do_POST(self):
            if self.path.startswith('/v1/messages/count_tokens'):
                return super().do_POST()
            codex = self.path.startswith('/v1/responses')
            if not codex and not self.path.startswith('/v1/messages'):
                return super().do_POST()
            cls = type(self)
            self._record_path()
            body = self._json_body()
            self._capture('codex-cli' if codex else 'claude-code', body)
            cls.error_requests += 1
            cls.error_observations.append({'body_sha256': hashlib.sha256(json.dumps(body, sort_keys=True).encode()).hexdigest(), 'stream': body.get('stream'), 'thinking': body.get('thinking'), 'output_config': body.get('output_config'), 'sdk_retry_count': self.headers.get('x-stainless-retry-count')})
            if cls.success_control:
                name = 'openai-response.sse' if codex else 'anthropic-response.sse'
                return self._send(200, 'text/event-stream', (self.fixture_dir / name).read_bytes())
            payload = json.dumps({'type': 'error', 'error': {'type': 'invalid_request_error', 'code': 'invalid_request_error', 'message': cls.error_marker + ':' + str(cls.error_requests)}, 'request_id': 'fixture-error-' + str(cls.error_requests)}).encode()
            cls.error_payloads.append(payload.decode())
            return self._send(400, 'application/json', payload)

    return ErrorHandler
