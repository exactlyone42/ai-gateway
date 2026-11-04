import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer
import time


class Handler(BaseHTTPRequestHandler):
    def respond(self, payload):
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header('content-type', 'application/json')
        self.send_header('content-length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.respond({})

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get('content-length', '0')))
        if (
            self.path != '/v1/chat/completions'
            or self.headers.get('authorization') != 'Bearer fixture-provider-key'
            or b'backup-e2e-' not in body
        ):
            detail = json.dumps({
                'path': self.path,
                'credential_matches': self.headers.get('authorization') == 'Bearer fixture-provider-key',
                'request_matches': b'backup-e2e-' in body,
            }).encode()
            self.send_response(400)
            self.send_header('content-length', str(len(detail)))
            self.end_headers()
            self.wfile.write(detail)
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
