import threading


def handler(base):
    class CancelHandler(base):
        stream_started = threading.Event()
        peer_closed = threading.Event()
        stop_stream = threading.Event()
        cancel_requests = 0
        client_exit = None

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
            cls.cancel_requests += 1
            if not body.get('stream'):
                return self._send(422, 'application/json', b'{"error":"stream_required"}')
            name = 'openai-response.sse' if codex else 'anthropic-response.sse'
            parts = (self.fixture_dir / name).read_text().split('\n\n')
            opening = '\n\n'.join(parts[:4 if codex else 3]) + '\n\n'
            opening = opening.replace('W06_CODEX_OK', 'CANCEL_STREAM_PENDING').replace('W05_CLAUDE_CODE_OK', 'CANCEL_STREAM_PENDING')
            self.send_response(200)
            self.send_header('content-type', 'text/event-stream')
            self.send_header('connection', 'close')
            self.end_headers()
            try:
                self.wfile.write(opening.encode())
                self.wfile.flush()
                cls.stream_started.set()
                for _ in range(400):
                    if cls.stop_stream.wait(0.1):
                        break
                    self.wfile.write(b': fixture-heartbeat\n\n')
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                cls.peer_closed.set()

    return CancelHandler
