import json
import os
import shlex
import signal
import subprocess
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def request_json(url, body=None):
    request = Request(url, data=json.dumps(body).encode() if body else None, headers={'content-type': 'application/json'})
    with urlopen(request, timeout=3) as response:
        return json.load(response)


def find_runner(blob):
    result = subprocess.run(['ps', '-C', 'llama-server', '-o', 'pid=,args='], capture_output=True, text=True, timeout=3)
    if result.returncode not in (0, 1):
        raise RuntimeError('無法核對本機模型程序')
    matches = []
    for line in result.stdout.splitlines():
        words = shlex.split(line)
        if '--model' in words and words[words.index('--model') + 1] == blob:
            if '--host' not in words or words[words.index('--host') + 1] != '127.0.0.1':
                raise RuntimeError('模型程序不是本機隔離入口')
            matches.append({'pid': int(words[0]), 'port': int(words[words.index('--port') + 1])})
    if len(matches) > 1:
        raise RuntimeError('同一模型有多個程序，不能判定本次取消')
    return matches[0] if matches else None


def slots(runner):
    data = request_json(f'http://127.0.0.1:{runner["port"]}/slots')
    if not isinstance(data, list) or len(data) != 1 or not isinstance(data[0].get('is_processing'), bool):
        raise RuntimeError('取消驗收需要唯一且可讀取狀態的推理工作')
    row = data[0]
    token = row.get('next_token') or {}
    if isinstance(token, list):
        if len(token) != 1 or not isinstance(token[0], dict):
            raise RuntimeError('推理工作回傳未知的計數格式')
        token = token[0]
    return {'id': row['id'], 'id_task': row['id_task'], 'is_processing': row['is_processing'], 'n_decoded': token.get('n_decoded', 0)}


def has_text(path):
    if path.stat().st_size > 1024 * 1024:
        raise RuntimeError('原Client取消前輸出超過測試上限')
    lines = path.read_text().splitlines()
    for index, line in enumerate(lines):
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            if index == len(lines) - 1:
                continue
            raise RuntimeError('原Client串流不是逐行JSON')
        delta = event.get('event', {}).get('delta', {})
        if event.get('type') == 'stream_event' and delta.get('type') == 'text_delta' and delta.get('text'):
            return True
    return False


def run(command, environment, runtime, home, model, codex, force_kill=False):
    show = request_json('http://127.0.0.1:11434/api/show', {'model': model})
    blob = next(line[5:].strip() for line in show['modelfile'].splitlines() if line.startswith('FROM '))
    runner = find_runner(blob)
    before = slots(runner) if runner else None
    proof = {'model': model, 'blob': blob, 'before': before}
    (home / 'local-cancel-proof.json').write_text(json.dumps(proof, indent=2))
    if before and before['is_processing']:
        raise RuntimeError('指定模型正在處理其他工作，未啟動驗收請求')
    output = home / 'client-events.jsonl'
    errors = home / 'client-stderr.txt'
    with output.open('w') as stdout, errors.open('w') as stderr:
        process = subprocess.Popen(command, env=environment, cwd=runtime, stdout=stdout, stderr=stderr, text=True, start_new_session=True)
        try:
            deadline = time.monotonic() + 240
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError('原Client在取消前已自行退出')
                current = find_runner(blob)
                if current:
                    try:
                        during = slots(current)
                    except HTTPError as error:
                        if error.code != 503:
                            raise
                    else:
                        if during['is_processing'] and during['n_decoded'] >= 8 and (codex or has_text(output)):
                            runner = current
                            break
                time.sleep(0.5)
            else:
                raise RuntimeError('期限內未觀察到原Client的實際生成')
            proof.update({'runner': runner, 'during': during, 'claude_text_observed': has_text(output) if not codex else None})
            (home / 'local-cancel-proof.json').write_text(json.dumps(proof, indent=2))
            started = time.monotonic()
            os.killpg(process.pid, signal.SIGKILL if force_kill else signal.SIGINT)
            process.wait(timeout=5)
            proof['client_exit'] = process.returncode
            events = [json.loads(line) for line in output.read_text().splitlines() if line.strip()] if codex else []
            kinds = {event.get('type') for event in events}
            interrupted = codex and process.returncode == 1 and {'thread.started', 'turn.started'}.issubset(kinds) and not kinds.intersection({'error', 'turn.failed', 'turn.completed'})
            if process.returncode not in (0, 130, -signal.SIGINT) and not interrupted:
                raise RuntimeError(f'原Client未正常取消：{process.returncode}')
            for _ in range(50):
                after = slots(runner)
                if not after['is_processing'] and time.monotonic() - started <= 10:
                    break
                if time.monotonic() - started > 10:
                    raise RuntimeError('取消後模型的推理工作未在10秒內釋放')
                time.sleep(0.1)
            else:
                raise RuntimeError('取消後模型的推理工作仍未釋放')
            proof.update({'after': after, 'stop_seconds': time.monotonic() - started})
            return subprocess.CompletedProcess(command, process.returncode, output.read_text(), errors.read_text()), proof
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=3)
            (home / 'local-cancel-proof.json').write_text(json.dumps(proof, indent=2))
