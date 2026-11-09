import hashlib
import json
import time


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
        stream_failure = False
        error_request_ids = []

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
            request_id = 'fixture-error-' + str(cls.error_requests)
            cls.error_request_ids.append(request_id)
            if cls.stream_failure:
                if not body.get('stream'):
                    return self._send(422, 'application/json', b'{"error":"stream_required"}')
                name = 'openai-response.sse' if codex else 'anthropic-response.sse'
                parts = (self.fixture_dir / name).read_text().split('\n\n')
                opening = '\n\n'.join(parts[:4 if codex else 3]) + '\n\n'
                opening = opening.replace('W06_CODEX_OK', cls.error_marker).replace('W05_CLAUDE_CODE_OK', cls.error_marker)
                payload = opening.encode()
                cls.error_payloads.append(opening)
                self.send_response(200)
                self.send_header('content-type', 'text/event-stream')
                self.send_header('content-length', str(len(payload) + 64))
                self.send_header('connection', 'close')
                self.send_header('x-should-retry', 'false')
                self.send_header('request-id', request_id)
                self.end_headers()
                self.wfile.write(payload)
                self.wfile.flush()
                time.sleep(0.25)
                self.close_connection = True
                return
            kind = 'authentication_error' if cls.auth_recovery else 'invalid_request_error'
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
