import json
from pathlib import Path
import queue
import subprocess
import threading
import tempfile
import time

root = Path(__file__).resolve().parents[4]
out = Path(__file__).resolve().parent
messages = queue.Queue()
records = []
started = time.monotonic()
with (out / 'server.stderr').open('w') as errors:
    server = subprocess.Popen(['codex', 'app-server', '--stdio', '-c', 'analytics.enabled=false'], cwd=root, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors, text=True)
    def read_messages():
        for line in server.stdout:
            try:
                messages.put(json.loads(line))
            except ValueError:
                messages.put({'unparsed': line.rstrip()})
    threading.Thread(target=read_messages, daemon=True).start()
    def send(request_id, method, params):
        server.stdin.write(json.dumps({'id': request_id, 'method': method, 'params': params}) + '\n')
        server.stdin.flush()
    def receive(predicate, timeout=15):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            item = messages.get(timeout=max(0.01, deadline - time.monotonic()))
            item['observed_elapsed'] = round(time.monotonic() - started, 4)
            records.append(item)
            if predicate(item):
                return item
        raise TimeoutError('No matching response')
    checks = {}
    try:
        send(1, 'initialize', {'clientInfo': {'name': 'platform_check', 'version': '0.1.0'}, 'capabilities': {'experimentalApi': True}})
        initial = receive(lambda item: item.get('id') == 1)
        checks['initialize'] = 'result' in initial
        server.stdin.write(json.dumps({'method': 'initialized'}) + '\n')
        server.stdin.flush()
        send(2, 'command/exec', {'command': ['rg', '-l', '-F', 'Provider 憑證永不外流', 'docs/architecture.md'], 'cwd': str(root), 'sandboxPolicy': {'type': 'readOnly'}, 'timeoutMs': 5000})
        found = receive(lambda item: item.get('id') == 2)
        checks['find_document'] = found.get('result', {}).get('exitCode') == 0 and 'docs/architecture.md' in found.get('result', {}).get('stdout', '')
        send(3, 'command/exec', {'command': ['rg', '-l', '-F', 'NONEXISTENT_PLATFORM_SENTINEL_20261024', 'docs/architecture.md'], 'cwd': str(root), 'sandboxPolicy': {'type': 'readOnly'}, 'timeoutMs': 5000})
        missing = receive(lambda item: item.get('id') == 3)
        checks['missing_document_rejected'] = missing.get('result', {}).get('exitCode') == 1 and not missing.get('result', {}).get('stdout')
        marker_dir = Path(tempfile.mkdtemp(prefix='cancel-check-', dir=out))
        marker = marker_dir / 'cancelled.txt'
        control = marker_dir / 'control.txt'
        policy = {'type': 'workspaceWrite', 'writableRoots': [str(marker_dir)], 'networkAccess': False}
        send(7, 'command/exec', {'command': ['python3', '-c', 'from pathlib import Path; Path(' + repr(str(control)) + ').write_text("completed")'], 'sandboxPolicy': policy, 'timeoutMs': 5000})
        control_response = receive(lambda item: item.get('id') == 7)
        checks['uncancelled_control_writes'] = control_response.get('result', {}).get('exitCode') == 0 and control.exists()
        send(4, 'command/exec', {'command': ['python3', '-u', '-c', 'import time; from pathlib import Path; print("READY",flush=True); time.sleep(5); Path(' + repr(str(marker)) + ').write_text("completed")'], 'processId': 'cancel-proof', 'streamStdoutStderr': True, 'sandboxPolicy': policy, 'timeoutMs': 10000})
        receive(lambda item: item.get('method') == 'command/exec/outputDelta' and item.get('params', {}).get('processId') == 'cancel-proof')
        cancel_start = time.monotonic()
        send(5, 'command/exec/terminate', {'processId': 'cancel-proof'})
        responses = {}
        while len(responses) < 2:
            response = receive(lambda item: item.get('id') in [4, 5], timeout=10)
            responses[response['id']] = response
        checks['cancel_acknowledged'] = 'result' in responses[5]
        checks['cancel_exit_nonzero'] = responses[4].get('result', {}).get('exitCode') not in [None, 0]
        checks['cancel_before_completion'] = time.monotonic() - cancel_start < 2
        time.sleep(5.2)
        checks['cancel_prevents_delayed_write'] = not marker.exists()
        send(6, 'command/exec/terminate', {'processId': 'no-such-process'})
        absent = receive(lambda item: item.get('id') == 6)
        checks['unknown_process_rejected'] = 'error' in absent
    except Exception as exc:
        checks['error'] = str(exc)
    finally:
        server.stdin.close()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.terminate()
            server.wait(timeout=5)
    result = {'checks': checks, 'seconds': round(time.monotonic() - started, 3), 'records': records, 'scope': 'Local app-server command execution only; no model turn or Gateway request'}
    (out / 'tool-results.json').write_text(json.dumps(result, ensure_ascii=False, indent=2))
    print(json.dumps({'checks': checks, 'seconds': result['seconds']}, ensure_ascii=False))
