import http.client
import json
from pathlib import Path
import socket
import threading
import time
import uuid


RSS_LIMIT = 32 * 1024 * 1024
SOURCE_LIMIT = 64 * 1024 * 1024


def handler(base):
    class SlowHandler(base):
        stream_started = threading.Event()
        peer_closed = threading.Event()
        stop_stream = threading.Event()
        cancel_requests = 0
        client_exit = None
        sent_bytes = 0
        source_finished = False
        source_error = None

        def do_POST(self):
            cls = type(self)
            codex = self.path.startswith('/v1/responses')
            if cls.cancel_requests or (not codex and not self.path.startswith('/v1/messages')) or self.path.startswith('/v1/messages/count_tokens'):
                return super().do_POST()
            cls.cancel_requests += 1
            self._record_path()
            self._capture('codex-cli' if codex else 'claude-code', self._json_body())
            self.connection.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 16 * 1024)
            self.connection.settimeout(25)
            name = 'openai-response.sse' if codex else 'anthropic-response.sse'
            parts = (self.fixture_dir / name).read_text().split('\n\n')
            opening = ('\n\n'.join(parts[:4 if codex else 3]) + '\n\n').replace('W06_CODEX_OK', 'CANCEL_STREAM_PENDING').replace('W05_CLAUDE_CODE_OK', 'CANCEL_STREAM_PENDING').encode()
            self.send_response(200)
            self.send_header('content-type', 'text/event-stream')
            self.send_header('connection', 'close')
            self.end_headers()
            frame = b': ' + b'x' * (64 * 1024 - 4) + b'\n\n'
            try:
                self.wfile.write(opening)
                self.wfile.flush()
                cls.sent_bytes += len(opening)
                cls.stream_started.set()
                for _ in range(SOURCE_LIMIT // len(frame)):
                    if cls.stop_stream.is_set():
                        return
                    self.wfile.write(frame)
                    self.wfile.flush()
                    cls.sent_bytes += len(frame)
                cls.source_finished = True
            except (BrokenPipeError, ConnectionResetError):
                cls.peer_closed.set()
            except TimeoutError:
                cls.source_error = 'write_timeout'
    return SlowHandler


def sample_process(pid):
    status = (Path('/proc') / str(pid) / 'status').read_text()
    rss = next(int(line.split()[1]) * 1024 for line in status.splitlines() if line.startswith('VmRSS:'))
    return {'rss': rss, 'fds': len(list((Path('/proc') / str(pid) / 'fd').iterdir()))}


def validate_pressure(proof):
    samples = proof['samples']
    for phase in ('paused', 'paused_again'):
        window = [row for row in samples if row['phase'] == phase][-8:]
        if len(window) != 8 or window[-1]['seconds'] - window[0]['seconds'] < 1.5 or len({row['source_bytes'] for row in window}) != 1:
            raise RuntimeError('來源未受到慢讀背壓')
    before = [row for row in samples if row['phase'] == 'paused'][-1]['source_bytes']
    after = [row for row in samples if row['phase'] == 'paused_again'][0]['source_bytes']
    if not 64 * 1024 <= before < after or proof['read_bytes'] <= 0 or proof['source_finished']:
        raise RuntimeError('未證明慢讀後來源恢復進度，或來源已耗盡')
    if max(row['source_bytes'] - row.get('read_bytes', 0) for row in samples) > 8 * 1024 * 1024:
        raise RuntimeError('來源尚未被Client讀取的資料超過8MiB上限')
    if max(row['rss'] for row in samples) - proof['baseline']['rss'] > RSS_LIMIT:
        raise RuntimeError('慢讀期間Gateway記憶體超過32MiB增量上限')


def validate_prefix(provider, client):
    strip = lambda value: '\n\n'.join(frame for frame in value.split('\n\n') if frame != ': keepalive')
    left, right = strip(provider), strip(client)
    if not right or not left.startswith(right):
        raise RuntimeError('慢讀取消的Client保存不是來源原文前綴')


def run(wrapper_pid, binary, base, credential, model, codex, fixture, runtime, drain=False):
    children = (Path('/proc') / str(wrapper_pid) / 'task' / str(wrapper_pid) / 'children').read_text().split()
    candidates = [int(value) for value in [str(wrapper_pid), *children] if (Path('/proc') / value / 'exe').resolve() == binary.resolve()]
    if len(candidates) != 1:
        raise RuntimeError('無法唯一識別Gateway執行檔，不能量測timeout父程序')
    pid = candidates[0]
    proof = {'pid': pid, 'wrapper_pid': wrapper_pid, 'binary': str(binary), 'baseline': sample_process(pid), 'samples': [], 'read_bytes': 0, 'native_cancel_run': False, 'reader_kind': 'raw_http_capacity_probe'}
    request_id = str(uuid.uuid4())
    payload = {'model': model, 'stream': True, **({'input': 'SLOW_READER_RESOURCE_CHECK', 'max_output_tokens': 16} if codex else {'messages': [{'role': 'user', 'content': 'SLOW_READER_RESOURCE_CHECK'}], 'max_tokens': 16})}
    body = json.dumps(payload).encode()
    address = base.removeprefix('http://').split(':')
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_WINDOW_CLAMP, 4096)
    sock.settimeout(5)
    response = None
    drainer = None
    stopped = threading.Event()
    started = time.monotonic()
    try:
        sock.connect((address[0], int(address[1])))
        proof['receive_buffer'] = sock.getsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF)
        path = '/v1/responses' if codex else '/v1/messages'
        key_header = 'Authorization: Bearer ' if codex else 'X-Api-Key: '
        headers = f'POST {path} HTTP/1.1\r\nHost: gateway\r\n{key_header}{credential}\r\nX-Request-Id: {request_id}\r\nContent-Type: application/json\r\nContent-Length: {len(body)}\r\nConnection: close\r\n\r\n'
        sock.sendall(headers.encode() + body)
        response = http.client.HTTPResponse(sock)
        response.begin()
        if response.status != 200 or not fixture.stream_started.wait(3):
            raise RuntimeError('慢讀測試未開始唯一來源串流')
        if drain:
            def read_fast():
                try:
                    while not stopped.is_set():
                        chunk = response.read(64 * 1024)
                        if not chunk:
                            return
                        proof['read_bytes'] += len(chunk)
                except (OSError, http.client.HTTPException) as error:
                    proof['drain_exit'] = type(error).__name__
            drainer = threading.Thread(target=read_fast, daemon=True)
            drainer.start()
        for phase, seconds in (('paused', 6), ('slow', 3), ('paused_again', 6)):
            if phase == 'paused_again' and not drain:
                before = fixture.sent_bytes
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 256 * 1024)
                sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_WINDOW_CLAMP, 256 * 1024)
                resumed = 0
                deadline = time.monotonic() + 5
                while fixture.sent_bytes == before and resumed < 4 * 1024 * 1024 and time.monotonic() < deadline:
                    data = response.read(64 * 1024)
                    if not data:
                        raise RuntimeError('來源在恢復讀取前已結束')
                    resumed += len(data)
                    proof['read_bytes'] += len(data)
                    proof['samples'].append({'phase': 'resume', 'seconds': time.monotonic() - started, 'source_bytes': fixture.sent_bytes, 'read_bytes': proof['read_bytes'], **sample_process(pid)})
                proof['resume_bytes'] = resumed
                proof['receive_buffer_after_resume'] = sock.getsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF)
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
                sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_WINDOW_CLAMP, 4096)
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if phase == 'slow' and not drain:
                    proof['read_bytes'] += len(response.read(16 * 1024))
                proof['samples'].append({'phase': phase, 'seconds': time.monotonic() - started, 'source_bytes': fixture.sent_bytes, 'read_bytes': proof['read_bytes'], **sample_process(pid)})
                if fixture.sent_bytes - proof['read_bytes'] > 8 * 1024 * 1024 or (drain and fixture.sent_bytes > 8 * 1024 * 1024) or fixture.peer_closed.is_set() or fixture.source_error:
                    raise RuntimeError('來源未受背壓或在取消前提前失敗')
                time.sleep(.25)
        proof['source_finished'] = fixture.source_finished
        validate_pressure(proof)
        proof['ingress_request_id'] = request_id
    finally:
        stopped.set()
        try:
            sock.shutdown(socket.SHUT_RDWR)
        except OSError as error:
            proof['close_errno'] = error.errno
        if drainer:
            drainer.join(timeout=3)
        if response:
            response.close()
        sock.close()
        proof['source_closed_after_cancel'] = fixture.peer_closed.wait(5)
        proof['after_cancel'] = sample_process(pid)
        proof['source_bytes'] = fixture.sent_bytes
        proof['source_error'] = fixture.source_error
        proof['source_finished'] = fixture.source_finished
        (runtime / 'slow-reader-result.json').write_text(json.dumps(proof, indent=2))
    if not proof['source_closed_after_cancel'] or proof['after_cancel']['fds'] > proof['baseline']['fds'] + 16:
        raise RuntimeError('慢讀取消後來源或檔案描述符未釋放')
    return proof
