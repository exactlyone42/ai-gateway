#!/usr/bin/env python3
import argparse
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request


def execute(command, **kwargs):
    result = subprocess.run(command, capture_output=True, timeout=30, **kwargs)
    if result.returncode:
        raise RuntimeError(result.stderr.decode(errors='replace'))
    return result.stdout


def free_port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--evidence')
    args = parser.parse_args()
    evidence = Path(args.evidence) if args.evidence else Path(tempfile.mkdtemp(prefix='caddy-cutover-'))
    evidence.mkdir(parents=True, exist_ok=True, mode=0o700)
    evidence.chmod(0o700)
    proceed = threading.Event()
    first = threading.Event()

    class Fixture(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == '/stream':
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.end_headers()
                self.wfile.write(b'data: old-start\n\n')
                self.wfile.flush()
                if not proceed.wait(15):
                    return
                self.wfile.write(b'data: old-end\n\n')
                self.wfile.flush()
            else:
                payload = self.server.label.encode()
                self.send_response(200)
                self.send_header('Content-Length', str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        def log_message(self, *args):
            pass

    old = ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
    new = ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
    old.label = 'old'
    new.label = 'new'
    for server in (old, new):
        threading.Thread(target=server.serve_forever, daemon=True).start()
    listener_port = free_port()
    admin_port = free_port()
    while admin_port == listener_port:
        admin_port = free_port()
    config_file = evidence / 'Caddyfile'

    def config(port):
        return ('{\n admin 127.0.0.1:' + str(admin_port) + '\n auto_https off\n}\n'
                'http://127.0.0.1:' + str(listener_port) + ' {\n'
                ' reverse_proxy 127.0.0.1:' + str(port) + '\n}\n')

    config_file.write_text(config(old.server_port))
    image = execute(['docker', 'inspect', 'caddy', '--format', '{{.Image}}']).decode().strip()
    name = 'gateway-caddy-cutover-' + str(time.time_ns())
    created = False
    base = 'http://127.0.0.1:' + str(listener_port)

    def get():
        with urllib.request.urlopen(base, timeout=5) as response:
            return response.read().decode()

    def stream():
        with urllib.request.urlopen(base + '/stream', timeout=20) as response:
            start = response.readline()
            if start != b'data: old-start\n':
                raise AssertionError(start)
            first.set()
            return start + response.read()

    def reload():
        return execute(['docker', 'exec', name, 'caddy', 'reload', '--config',
                        '/etc/caddy/Caddyfile', '--address', '127.0.0.1:' + str(admin_port)])

    try:
        execute(['docker', 'run', '-d', '--name', name, '--network', 'host', '--cpus', '0.5',
                 '--memory', '128m', '-v', str(config_file) + ':/etc/caddy/Caddyfile:ro', image])
        created = True
        deadline = time.monotonic() + 15
        while True:
            try:
                if get() == 'old':
                    break
            except (OSError, urllib.error.URLError):
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.1)
        execute(['docker', 'exec', '-i', name, 'caddy', 'validate', '--config', '-',
                 '--adapter', 'caddyfile'], input=config(new.server_port).encode())
        with ThreadPoolExecutor(max_workers=1) as pool:
            pending = pool.submit(stream)
            if not first.wait(10):
                raise RuntimeError('舊版串流未送出首段')
            config_file.write_text(config(new.server_port))
            reload()
            assert get() == 'new', '切換後新請求未進新版'
            assert not pending.done(), '切換截斷舊版串流'
            proceed.set()
            body = pending.result(timeout=10)
            assert body == b'data: old-start\n\ndata: old-end\n\n', body
            (evidence / 'stream.txt').write_bytes(body)
        config_file.write_text('not_a_valid_caddyfile {\n invalid_directive\n}\n')
        rejected = subprocess.run(['docker', 'exec', name, 'caddy', 'reload', '--config',
                                  '/etc/caddy/Caddyfile', '--address', '127.0.0.1:' + str(admin_port)],
                                  capture_output=True, timeout=15)
        assert rejected.returncode != 0, '錯誤設定未被拒絕'
        assert get() == 'new', '錯誤設定破壞現有服務'
        (evidence / 'invalid-reload.err').write_bytes(rejected.stderr)
        receipt = {'image': image, 'container': name, 'stdin_validate': True,
                   'old_sse_completed': True, 'new_requests_switched': True,
                   'invalid_reload_preserved_service': True}
        (evidence / 'result.json').write_text(json.dumps(receipt, indent=2))
        print(json.dumps({'passed': True, 'evidence': str(evidence), **receipt}))
    finally:
        proceed.set()
        if created:
            logs = subprocess.run(['docker', 'logs', name], capture_output=True, timeout=10)
            (evidence / 'caddy.log').write_bytes(logs.stdout + logs.stderr)
            execute(['docker', 'stop', '--time', '5', name])
        for server in (old, new):
            server.shutdown()
            server.server_close()


if __name__ == '__main__':
    main()
