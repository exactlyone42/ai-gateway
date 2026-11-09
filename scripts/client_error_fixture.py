import hashlib
import json


def handler(base):
    class ErrorHandler(base):
        error_requests = 0
        error_observations = []
        error_marker = 'fixture-upstream-request-rejected'
        error_payloads = []
        success_control = False
        auth_recovery = False
        recovered = False
        recovery_requests = 0

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
            if cls.recovered:
                cls.recovery_requests += 1
                name = 'openai-response.sse' if codex else 'anthropic-response.sse'
                return self._send(200, 'text/event-stream', (self.fixture_dir / name).read_bytes())
            cls.error_requests += 1
            cls.error_observations.append({'body_sha256': hashlib.sha256(json.dumps(body, sort_keys=True).encode()).hexdigest(), 'stream': body.get('stream'), 'thinking': body.get('thinking'), 'output_config': body.get('output_config'), 'sdk_retry_count': self.headers.get('x-stainless-retry-count')})
            if cls.success_control:
                name = 'openai-response.sse' if codex else 'anthropic-response.sse'
                return self._send(200, 'text/event-stream', (self.fixture_dir / name).read_bytes())
            kind = 'authentication_error' if cls.auth_recovery else 'invalid_request_error'
            request_id = 'fixture-error-' + str(cls.error_requests)
            payload = json.dumps({'type': 'error', 'error': {'type': kind, 'code': kind, 'message': cls.error_marker + ':' + str(cls.error_requests)}, 'request_id': request_id}).encode()
            cls.error_payloads.append(payload.decode())
            if cls.auth_recovery:
                self.send_response(401)
                self.send_header('content-type', 'application/json')
                self.send_header('content-length', str(len(payload)))
                self.send_header('connection', 'close')
                self.send_header('x-should-retry', 'false')
                self.send_header('request-id', request_id)
                self.end_headers()
                self.wfile.write(payload)
                return
            return self._send(400, 'application/json', payload)

    return ErrorHandler
