import json
import threading
import time


def handler(base):
    class CancelHandler(base):
        stream_started = threading.Event()
        peer_closed = threading.Event()
        stop_stream = threading.Event()
        cancel_requests = 0
        client_exit = None
        stream_seconds = 0
        elapsed_seconds = None
        emitted_stream = ''

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
            if not cls.stream_seconds:
                opening = opening.replace('W06_CODEX_OK', 'CANCEL_STREAM_PENDING').replace('W05_CLAUDE_CODE_OK', 'CANCEL_STREAM_PENDING')
            self.send_response(200)
            self.send_header('content-type', 'text/event-stream')
            self.send_header('connection', 'close')
            self.end_headers()
            try:
                if cls.stream_seconds:
                    prefix_count = 3 if codex else 2
                    delta = json.loads(parts[prefix_count].split('data: ', 1)[1])
                    text = delta['delta'] if codex else delta['delta']['text']
                    frames = [('\n\n'.join(parts[:prefix_count]) + '\n\n', 0)]
                    for index, character in enumerate(text):
                        event = delta | {'delta': character, 'sequence_number': index + prefix_count} if codex else delta | {'delta': {'type': 'text_delta', 'text': character}}
                        frames.append((('' if codex else 'event: content_block_delta\n') + 'data: ' + json.dumps(event) + '\n\n', cls.stream_seconds / len(text)))
                    for part in parts[prefix_count + 1:]:
                        if part:
                            if codex:
                                event = json.loads(part.split('data: ', 1)[1])
                                event['sequence_number'] += len(text) - 1
                                part = 'data: ' + json.dumps(event)
                            frames.append((part + '\n\n', 0))
                    started = time.monotonic()
                    for payload, delay in frames:
                        self.wfile.write(payload.encode())
                        self.wfile.flush()
                        cls.emitted_stream += payload
                        cls.stream_started.set()
                        if delay and cls.stop_stream.wait(delay):
                            return
                    cls.elapsed_seconds = time.monotonic() - started
                    return
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
