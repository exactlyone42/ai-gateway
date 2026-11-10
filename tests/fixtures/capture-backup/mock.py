import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
import time


class Handler(BaseHTTPRequestHandler):
    requests = 0
    captured_requests = 0

    def respond(self, payload):
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header('content-type', 'application/json')
        self.send_header('content-length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.respond({'requests': Handler.requests, 'captured_requests': Handler.captured_requests})

    def do_POST(self):
        Handler.requests += 1
        body = self.rfile.read(int(self.headers.get('content-length', '0')))
        authenticated = self.headers.get('x-api-key') == 'fixture-provider-key' if self.path == '/v1/messages' else self.headers.get('authorization') == 'Bearer fixture-provider-key'
        if (
            self.path not in ('/v1/chat/completions', '/v1/responses', '/v1/messages')
            or not authenticated
            or b'backup-e2e-' not in body
        ):
            detail = json.dumps({
                'path': self.path,
                'credential_matches': authenticated,
                'request_matches': b'backup-e2e-' in body,
            }).encode()
            self.send_response(400)
            self.send_header('content-length', str(len(detail)))
            self.end_headers()
            self.wfile.write(detail)
            return
        if os.environ.get('REQUIRE_CAPTURED_REQUEST') == '1' and b'backup-e2e-after-restore' not in body:
            pending = 0
            files = list(Path('/capture-spool').glob('*.capture'))
            if len(files) > 16:
                raise RuntimeError('too many capture fixture files')
            for path in files:
                try:
                    with path.open('rb') as source:
                        data = source.read(2 * 1024 * 1024 + 1)
                except FileNotFoundError:
                    continue
                if len(data) > 2 * 1024 * 1024 or data[:8] != b'APGCAP06' or b'backup-e2e-' in data:
                    raise RuntimeError('capture fixture is invalid or contains plaintext')
                position = 158
                legs = []
                while position + 21 <= len(data):
                    legs.append(data[position])
                    length = int.from_bytes(data[position + 9:position + 13], 'big')
                    position += 21 + length
                if position != len(data):
                    raise RuntimeError('capture fixture contains a partial record')
                pending += legs == [0, 1]
            if pending != 1:
                raise RuntimeError('request was not captured before the provider call')
            Handler.captured_requests += 1
        fixture = {'/v1/responses': 'openai-response.sse', '/v1/messages': 'anthropic-response.sse'}.get(self.path)
        if fixture is not None:
            payload = (Path('/fixtures') / fixture).read_bytes()
            self.send_response(200)
            self.send_header('content-type', 'text/event-stream')
            self.send_header('content-length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        if json.loads(body).get('stream'):
            self.send_response(200)
            self.send_header('content-type', 'text/event-stream')
            self.send_header('connection', 'close')
            self.end_headers()
            self.wfile.write(b'data: {"id":"fixture","object":"chat.completion.chunk","created":1,"model":"company-default","choices":[{"index":0,"delta":{"content":"fixture start"},"finish_reason":null}]}\n\n')
            self.wfile.flush()
            time.sleep(float(os.environ.get('FIXTURE_STREAM_DELAY', '3')))
            self.wfile.write(b'data: {"id":"fixture","object":"chat.completion.chunk","created":1,"model":"company-default","choices":[{"index":0,"delta":{"content":"fixture end"},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n')
            self.wfile.flush()
            return
        self.respond({
            'id': 'fixture',
            'object': 'chat.completion',
            'created': 1,
            'model': 'company-default',
            'choices': [{
                'index': 0,
                'message': {'role': 'assistant', 'content': 'fixture reply'},
                'finish_reason': 'stop',
            }],
            'usage': {'prompt_tokens': 1, 'completion_tokens': 1, 'total_tokens': 2},
        })


HTTPServer(('0.0.0.0', 8090), Handler).serve_forever()
