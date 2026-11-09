from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path
import subprocess
import time


def verify(api, create, activate, users, fixture, runtime, codex, database):
    root = Path(__file__).resolve().parent.parent
    endpoint = '/v1/responses' if codex else '/v1/messages'
    body = {'model': ('company' if codex else 'claude') + '-alice', 'stream': True}
    body.update({'input': 'ledger-capacity', 'max_output_tokens': 16} if codex else {'messages': [{'role': 'user', 'content': 'ledger-capacity'}], 'max_tokens': 16})
    stream = (root / 'tests/fixtures/client-roundtrip' / ('openai-response.sse' if codex else 'anthropic-response.sse')).read_bytes()
    terminal = b'response.completed' if codex else b'message_stop'
    if terminal not in stream:
        raise RuntimeError('受控來源缺少串流完成事件')
    expected_hash = hashlib.sha256(stream).hexdigest()
    estimate = len(json.dumps(body).encode()) + 16
    quota = create('quotas', {'quota_key': 'ledger-capacity', 'revision': 1, 'user_id': users['alice'][0], 'configuration': {'mode': 'hard', 'limits': {'tokens_per_hour': estimate * 2}}})['resource']
    activate('quotas', quota)

    def state(label):
        result = subprocess.run(['psql', database, '-XAtq', '-v', 'ON_ERROR_STOP=1'], input=(root / 'queries/test_capture_backup_state.sql').read_text(), capture_output=True, text=True, timeout=6, check=True)
        value = json.loads(result.stdout)
        (runtime / (label + '.json')).write_text(json.dumps(value, indent=2))
        return value

    def call(label):
        metadata = {}
        raw = api(endpoint, 'POST', body, users['alice'][1], expected=None, raw_response=True, response_metadata=metadata)
        row = {'case': label, **metadata, 'body_sha256': hashlib.sha256(raw.encode()).hexdigest()}
        if metadata['status'] != 200:
            row['error'] = json.loads(raw)['error']['code']
        return row

    fixture.arm()
    with ThreadPoolExecutor(max_workers=1) as worker:
        ongoing = worker.submit(call, 'in-flight')
        try:
            if not fixture.started.wait(3) or ongoing.done() or len(fixture.captured_requests) != 1:
                raise RuntimeError('來源尚未接到第一筆進行中請求')
            files = {path.name: path.stat().st_size for path in (runtime / 'ledger').iterdir() if path.suffix in ('.reserved', '.working', '.next')}
            if len(files) != 3 or any(size != 32768 for size in files.values()):
                raise RuntimeError('單筆進行中請求未占滿設定的帳本容量')
            (runtime / 'capacity-proof.json').write_text(json.dumps({'max_bytes': 98304, 'entry_max_bytes': 32768, 'files': files, 'first_source_calls': 1}, indent=2))
            rejected = call('ledger-full')
            source_after_rejection = len(fixture.captured_requests)
            state('while-full')
        finally:
            fixture.release_headers.set()
            fixture.release_body.set()
        first = ongoing.result(timeout=8)
    time.sleep(1)
    state('after-release')
    recovered = call('capacity-restored')
    calls = [first, rejected, recovered]
    (runtime / 'capacity-calls.json').write_text(json.dumps(calls, indent=2))
    time.sleep(1)
    final = state('final-state')
    if first['status'] != 200 or rejected['status'] != 503 or rejected.get('error') != 'ledger_unavailable' or source_after_rejection != 1:
        raise RuntimeError('帳本滿時未保留舊請求並拒絕新上游')
    if recovered['status'] != 200 or len(fixture.captured_requests) != 2:
        raise RuntimeError('帳本恢復後新請求仍被先前預留額度擋住')
    if first['body_sha256'] != expected_hash or recovered['body_sha256'] != expected_hash:
        raise RuntimeError('帳本容量故障期間的舊串流或恢復後新串流不完整')
    requests = {row['request_id']: row for row in final['gateway_requests']}
    reservations = {row['gateway_request_id']: row for row in final['quota_reservations']}
    checks = []
    for call, tokens in zip(calls, (2, 0, 2), strict=True):
        row = requests.get(call['request_id'])
        status = 'failed' if tokens == 0 else 'completed'
        if row is None or row['status'] != status:
            raise RuntimeError('帳本容量故障與恢復後紀錄不完整')
        reservation = reservations[row['id']]
        events = [event for event in final['quota_reconciliation_events'] if event['reservation_id'] == reservation['id']]
        attempts = [attempt for attempt in final['provider_attempts'] if attempt['gateway_request_id'] == row['id']]
        if reservation['state'] != 'reconciled' or reservation['actual_tokens'] != tokens or len(events) != 1 or events[0]['actual_tokens'] != tokens or len(attempts) != (1 if tokens else 0):
            raise RuntimeError('帳本容量故障後重複、漏結帳或用量不符')
        checks.append({'request_id': row['id'], 'status': status, 'actual_tokens': tokens, 'events': 1, 'attempts': len(attempts)})
    if sum(row['reserved_tokens'] for row in final['quota_windows'] if row['limit_kind'] == 'tokens_per_hour') != 4:
        raise RuntimeError('帳本容量故障留下未回收的額度估量')
    (runtime / 'capacity-result.json').write_text(json.dumps({'calls': calls, 'estimate': estimate, 'quota_limit': estimate * 2, 'final_tokens': 4, 'source_calls': 2, 'checks': checks, 'scope': '同機隔離HTTP與受控來源，非原Client或真模型'}, indent=2, ensure_ascii=False))
