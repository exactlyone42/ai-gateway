import hashlib
import json
import os
from pathlib import Path
import subprocess
import time


def verify(api, create, activate, users, fixture, runtime, admin, codex, database):
    root = Path(__file__).resolve().parent.parent
    endpoint = '/v1/responses' if codex else '/v1/messages'
    model = ('company' if codex else 'claude') + '-alice'
    body = {'model': model, 'stream': True}
    body.update({'input': 'quota-recovery', 'max_output_tokens': 16} if codex else {'messages': [{'role': 'user', 'content': 'quota-recovery'}], 'max_tokens': 16})
    estimate = len(json.dumps(body).encode()) + 16
    quota = create('quotas', {'quota_key': 'preflight-recovery', 'revision': 1, 'user_id': users['alice'][0], 'configuration': {'mode': 'hard', 'limits': {'tokens_per_hour': estimate + 2}}})['resource']
    activate('quotas', quota)
    rows = []

    def state(label):
        query = (root / 'queries/test_capture_backup_state.sql').read_text()
        result = subprocess.run(['psql', database, '-XAtq', '-v', 'ON_ERROR_STOP=1'], input=query, capture_output=True, text=True, timeout=6, check=True)
        value = json.loads(result.stdout)
        (runtime / (label + '.json')).write_text(json.dumps(value, indent=2))
        return value

    def call(label):
        metadata = {}
        before = len(fixture.captured_requests)
        raw = api(endpoint, 'POST', body, users['alice'][1], expected=None, raw_response=True, response_metadata=metadata)
        row = {'case': label, **metadata, 'source_calls': len(fixture.captured_requests) - before, 'body_sha256': hashlib.sha256(raw.encode()).hexdigest()}
        if metadata['status'] != 200:
            row['error'] = json.loads(raw)['error']['code']
        rows.append(row)
        (runtime / 'preflight-calls.json').write_text(json.dumps(rows, indent=2))
        return row

    def settled(label):
        time.sleep(1)
        return state(label)

    state('before-fault')
    spool = runtime / 'encrypted-spool'
    os.chmod(spool, 0o500)
    try:
        denied = call('capture-unavailable')
    finally:
        os.chmod(spool, 0o700)
    settled('after-fault')
    recovered = call('storage-restored')
    state('after-recovery-response')
    if denied['status'] != 503 or denied.get('error') != 'capture_unavailable' or denied['source_calls'] != 0:
        raise RuntimeError('保存故障未在送模型前拒絕')
    if recovered['status'] != 200 or recovered['source_calls'] != 1:
        raise RuntimeError('保存恢復後新請求仍被先前預留額度擋住')
    settled('after-first-recovery')
    provider_key = 'openai' if codex else 'anthropic'
    target = f'/admin/v1/provider-api-keys/{provider_key}/{users["alice"][0]}'
    api(target, 'DELETE', token=admin, expected=204)
    try:
        source_denied = call('source-unavailable')
    finally:
        api(target, 'PUT', {'token': fixture.provider_keys_by_model[model]}, admin)
    settled('after-source-fault')
    source_recovered = call('source-restored')
    if source_denied['status'] != 503 or source_denied.get('error') != 'provider_unavailable' or source_denied['source_calls'] != 0:
        raise RuntimeError('缺本人來源未在送模型前拒絕')
    if source_recovered['status'] != 200 or source_recovered['source_calls'] != 1:
        raise RuntimeError('本人來源恢復後新請求仍被先前預留額度擋住')
    final_state = settled('final-state')
    matched = [row for row in final_state['gateway_requests'] if row['request_id'] in {call['request_id'] for call in rows}]
    if len(matched) != 4 or not all(row['state'] == 'reconciled' for row in final_state['quota_reservations']):
        raise RuntimeError('失敗與恢復後請求尚未完整入帳或結帳')
    failed = [row for row in matched if row['request_id'] in (denied['request_id'], source_denied['request_id'])]
    healthy = [row for row in matched if row['request_id'] in (recovered['request_id'], source_recovered['request_id'])]
    if len(failed) != 2 or any(row['status'] != 'failed' for row in failed) or len(healthy) != 2 or any(row['status'] != 'completed' for row in healthy):
        raise RuntimeError('失敗與恢復的帳本狀態不正確')
    if any(row['gateway_request_id'] in {request['id'] for request in failed} for row in final_state['provider_attempts']):
        raise RuntimeError('尚未送出模型的失敗卻有上游嘗試')
    reservations = {row['gateway_request_id']: row for row in final_state['quota_reservations']}
    if any(reservations[row['id']]['actual_tokens'] != 0 for row in failed) or any(reservations[row['id']]['actual_tokens'] != 2 for row in healthy):
        raise RuntimeError('零上游失敗或恢復請求的實際用量不符')
    events = final_state['quota_reconciliation_events']
    if len(events) != 4 or sorted(row['actual_tokens'] for row in events) != [0, 0, 2, 2]:
        raise RuntimeError('失敗與恢復請求重複或漏結帳')
    windows = [row for row in final_state['quota_windows'] if row['limit_kind'] == 'tokens_per_hour']
    if sum(row['reserved_tokens'] for row in windows) != 4:
        raise RuntimeError('額度沒有回收失敗估量並只保留實際用量')
    result = {'cases': rows, 'estimate': estimate, 'quota_limit': estimate + 2, 'final_tokens': 4, 'actual_tokens': [0, 2, 0, 2], 'final_events': len(events), 'source_calls': len(fixture.captured_requests), 'scope': '同機隔離HTTP與受控來源，非原Client或真模型'}
    (runtime / 'preflight-result.json').write_text(json.dumps(result, indent=2, ensure_ascii=False))
