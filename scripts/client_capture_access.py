from concurrent.futures import ThreadPoolExecutor
from client_capture_validation import reject_plaintext
import hashlib
import http.client
import json
import os
from pathlib import Path
import subprocess
import threading
import time
from urllib.parse import urlsplit
import uuid


def verify_plaintext(runtime, database, secrets):
    markers = ['CAPTURE_REQUEST_', 'CAPTURE_SAVED_PREFIX', 'CAPTURE_UNSAVED_FRAME', 'W05_CLAUDE_CODE_OK', 'W06_CODEX_OK', *secrets]
    log = (runtime / 'gateway.log').read_bytes()
    reject_plaintext(log, markers)
    files = list((runtime / 'encrypted-spool').glob('*.capture'))
    for path in files:
        reject_plaintext(path.read_bytes(), markers)
    dump = subprocess.run(['pg_dump', database, '--data-only', '--no-owner', '--no-privileges'], capture_output=True, timeout=15, check=True)
    reject_plaintext(dump.stdout, markers)
    result = {'plaintext_absent': True, 'spool_files': len(files), 'database_dump_bytes': len(dump.stdout), 'database_dump_sha256': hashlib.sha256(dump.stdout).hexdigest(), 'gateway_log_bytes': len(log), 'markers_checked': len(markers), 'dump_not_saved': True}
    (runtime / 'capture-access-plaintext.json').write_text(json.dumps(result, indent=2))
    return result


def handler(base, spool):
    class CaptureFixture(base):
        stream_fault = False
        release_suffix = threading.Event()
        source_finished = threading.Event()
        suffix_sent = False
        source_closed = False
        source_proofs = []

        def _capture(self, name, body):
            super()._capture(name, body)
            files = list(spool.glob('*.capture'))
            if len(files) > 16:
                raise RuntimeError('受控來源看到過多保存檔案')
            pending = []
            for path in files:
                try:
                    with path.open('rb') as stream:
                        raw = stream.read(2 * 1024 * 1024 + 1)
                except FileNotFoundError:
                    continue
                if len(raw) > 2 * 1024 * 1024 or raw[:8] != b'APGCAP06' or b'CAPTURE_' in raw:
                    raise RuntimeError('受控來源看到格式錯誤或含明文的保存檔')
                position, legs = 158, []
                while position + 21 <= len(raw):
                    legs.append(raw[position])
                    position += 21 + int.from_bytes(raw[position + 9:position + 13], 'big')
                if position != len(raw):
                    raise RuntimeError('受控來源看到尚未寫完整的保存區塊')
                if legs == [0, 1]:
                    pending.append(path.stem)
            type(self).source_proofs.append({'capture_ids': pending, 'requests_saved_before_source': len(pending) == 1})
            if len(pending) != 1:
                raise RuntimeError('請求尚未保存就已送到來源')

        def do_POST(self):
            if not self.stream_fault:
                return super().do_POST()
            codex = self.path.startswith('/v1/responses')
            self._record_path()
            self._capture('codex-cli' if codex else 'claude-code', self._json_body())
            payload = (self.fixture_dir / ('openai-response.sse' if codex else 'anthropic-response.sse')).read_bytes()
            marker = b'W06_CODEX_OK' if codex else b'W05_CLAUDE_CODE_OK'
            end = payload.index(b'\n\n', payload.index(marker)) + 2
            prefix = payload[:end].replace(marker, b'CAPTURE_SAVED_PREFIX')
            event = {'type': 'response.output_text.delta', 'item_id': 'msg_fixture', 'output_index': 0, 'content_index': 0, 'delta': 'CAPTURE_UNSAVED_FRAME'} if codex else {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': 'CAPTURE_UNSAVED_FRAME'}}
            suffix = b'data: ' + json.dumps(event).encode() + b'\n\n' + payload[end:]
            try:
                self.send_response(200)
                self.send_header('content-type', 'text/event-stream')
                self.send_header('content-length', str(len(prefix) + len(suffix)))
                self.send_header('connection', 'close')
                self.end_headers()
                self.wfile.write(prefix)
                self.wfile.flush()
                if not self.release_suffix.wait(5):
                    raise RuntimeError('受控來源等待放行後段資料逾時')
                self.wfile.write(suffix)
                self.wfile.flush()
                type(self).suffix_sent = True
            except (BrokenPipeError, ConnectionResetError):
                type(self).source_closed = True
            finally:
                self.source_finished.set()
    return CaptureFixture


def verify(base, api, create, activate, users, provider, fixture, runtime, admin, other_admin, codex, database, control):
    prefix = 'company' if codex else 'claude'
    endpoint = '/v1/responses' if codex else '/v1/messages'
    model = prefix + '-alice'
    tenant_b_model = prefix + '-tenant-b'
    marker = 'CAPTURE_REQUEST_' + uuid.uuid4().hex
    rows, captures, seen = [], [], set()
    fixture.client_credentials.update([admin, other_admin])

    def save():
        (runtime / 'capture-access.json').write_text(json.dumps({'scope': '隔離HTTP與受控來源，未啟動原Client', 'control': control, 'cases': rows, 'captures': captures}, indent=2))

    def call(label, path, token, expected=200, method='GET', body=None, calls=0):
        before = len(fixture.captured_requests)
        metadata = {}
        raw = api(path, method, body, token, expected=None, raw_response=True, response_metadata=metadata)
        observed = fixture.captured_requests[before:]
        safe = all(secret not in raw for secret in fixture.provider_credentials | fixture.client_credentials)
        if expected >= 400:
            safe = safe and marker not in raw and 'CAPTURE_SAVED_PREFIX' not in raw
        row = {'case': label, 'status': metadata['status'], 'expected_status': expected, 'source_calls': len(observed), 'expected_source_calls': calls, 'gateway_request_id': metadata['request_id'], 'cache_control': metadata['cache_control'], 'secrets_absent': safe, 'body_sha256': hashlib.sha256(raw.encode()).hexdigest(), 'body_bytes': len(raw.encode())}
        rows.append(row)
        save()
        if row['status'] != expected or row['source_calls'] != calls or not safe or not row['gateway_request_id'] or not all(r['provider_credential_matches_fixture'] and r['client_credential_absent'] for r in observed):
            raise RuntimeError(f'內容保存驗收拒絕{label}：狀態={row["status"]}，來源次數={row["source_calls"]}')
        return raw

    def body(public, label):
        return {'model': public, 'stream': True, **({'input': marker + label, 'max_output_tokens': 64} if codex else {'messages': [{'role': 'user', 'content': marker + label}], 'max_tokens': 64})}

    def capture(user, expected='complete'):
        query = Path(__file__).resolve().parent.parent / 'queries/test_client_capture_records.sql'
        deadline = time.monotonic() + 40
        for _ in range(40):
            result = subprocess.run(['psql', database, '-XAtq', '-v', 'ON_ERROR_STOP=1', '-v', 'user=' + user], input=query.read_text(), capture_output=True, text=True, timeout=3, check=True)
            records = [r for r in json.loads(result.stdout) if r['capture_id'] not in seen]
            if len(records) == 1 and records[0]['state'] == expected:
                record = {**records[0], 'user_id': user}
                seen.add(record['capture_id'])
                captures.append(record)
                save()
                return record
            if len(records) > 1 or time.monotonic() >= deadline:
                break
            time.sleep(1)
        raise RuntimeError('新保存紀錄未在期限內以預期狀態入庫')

    def read_legs(record, token, label):
        content = {}
        for leg in ['client_request', 'provider_request', 'provider_response', 'client_response']:
            content[leg] = call(label + '-' + leg, f'/admin/v1/request-captures/{record["capture_id"]}/content?leg={leg}', token)
            if not content[leg] or rows[-1]['cache_control'] != 'no-store':
                raise RuntimeError('下載的保存內容為空，或未禁止快取')
        record['plaintext_leg_sha256'] = {leg: hashlib.sha256(value.encode()).hexdigest() for leg, value in content.items()}
        save()
        return content

    other_provider = create('providers', {'provider_key': provider['provider_key'], 'display_name': '租戶B保存來源', 'provider_type': provider['provider_type'], 'base_url': provider['base_url'], 'credential_ref': provider['credential_ref'], 'supported_operations': provider['supported_operations']}, other_admin)['resource']
    activate('providers', other_provider, other_admin)
    resource = create('models', {'provider_id': other_provider['id'], 'public_model_id': tenant_b_model, 'provider_model_id': tenant_b_model, 'supported_operations': provider['supported_operations'], 'supports_streaming': True}, other_admin)['resource']
    activate('models', resource, other_admin)
    other_user = create('users', {'display_name': '保存本人B'}, other_admin)['resource']
    other_key = create('credentials', {'display_name': '保存本人B', 'user_id': other_user['id']}, other_admin)['secret']
    fixture.client_credentials.add(other_key)
    fixture.provider_credentials.add('fixture-capture-tenant-b')
    fixture.provider_keys_by_model[tenant_b_model] = 'fixture-capture-tenant-b'
    policy = create('policies', {'policy_key': 'capture-b', 'revision': 1, 'user_id': other_user['id'], 'configuration': {'constraints': {'allowed_models': [tenant_b_model]}}}, other_admin)['resource']
    activate('policies', policy, other_admin)
    api(f'/admin/v1/provider-api-keys/{provider["provider_key"]}/{other_user["id"]}', 'PUT', {'token': 'fixture-capture-tenant-b'}, other_admin)
    normal = call('alice-stream', endpoint, users['alice'][1], method='POST', body=body(model, 'alice'), calls=1)
    alice_record = capture(users['alice'][0])
    alice_content = read_legs(alice_record, admin, 'admin-alice')
    if alice_content['provider_response'] != normal or alice_content['client_response'] != normal or marker + 'alice' not in alice_content['client_request']:
        raise RuntimeError('A的保存內容與實際傳輸不符')
    other_wire = call('tenant-b-stream', endpoint, other_key, method='POST', body=body(tenant_b_model, 'other'), calls=1)
    other_record = capture(other_user['id'])
    other_content = read_legs(other_record, other_admin, 'admin-b')
    if other_content['provider_response'] != other_wire or other_content['client_response'] != other_wire or marker + 'other' not in other_content['client_request'] or other_content['client_request'] == alice_content['client_request']:
        raise RuntimeError('租戶B的保存內容與自己的傳輸不符')
    target = f'/admin/v1/request-captures/{alice_record["capture_id"]}'
    for role in ['operator', 'viewer']:
        account = create('admin-accounts', {'display_name': role, 'credential_name': role, 'role': role})
        session = api('/admin/v1/sessions', 'POST', token=account['login_token'], expected=201)['access_token']
        fixture.client_credentials.update([session, account['login_token']])
        call(role + '-metadata-denied', target, session, 403)
        for leg in alice_content:
            token = admin if control == 'viewer' and role == 'viewer' and leg == 'client_request' else session
            call(role + '-' + leg + '-denied', target + '/content?leg=' + leg, token, 403)
    for label, token, path, expected in [('foreign-admin', other_admin, target, 404), ('alice-gateway-key', users['alice'][1], target, 401), ('bob-gateway-key', users['bob'][1], target, 401), ('no-session', None, target, 401), ('a-admin-b-capture', admin, f'/admin/v1/request-captures/{other_record["capture_id"]}', 404)]:
        call(label + '-metadata-denied', path, token, expected)
        call(label + '-content-denied', path + '/content?leg=client_request', token, expected)
    call('admin-still-reads-after-denials', target + '/content?leg=client_request', admin)
    spool = runtime / 'encrypted-spool'
    os.chmod(spool, 0o700 if control == 'before' else 0o500)
    try:
        denied = call('storage-fault-before-source', endpoint, users['alice'][1], 503, 'POST', body(model, 'before'), 0)
        if json.loads(denied)['error']['code'] != 'capture_unavailable':
            raise RuntimeError('請求並非因保存失敗而遭拒絕')
    finally:
        os.chmod(spool, 0o700)
    recovered = call('storage-restored-new-request', endpoint, users['alice'][1], method='POST', body=body(model, 'recovered'), calls=1)
    recovered_record = capture(users['alice'][0])
    content = read_legs(recovered_record, admin, 'admin-recovered')
    if content['client_response'] != recovered:
        raise RuntimeError('恢復後的回覆與保存內容不符')
    fixture.stream_fault = True
    prefix_seen = threading.Event()
    request_id = str(uuid.uuid4())
    url = urlsplit(base)

    def reader():
        connection = http.client.HTTPConnection(url.hostname, url.port, timeout=4)
        received = bytearray()
        error = None
        status = None
        try:
            connection.request('POST', endpoint, body=json.dumps(body(model, 'during')), headers={'authorization': 'Bearer ' + users['alice'][1], 'content-type': 'application/json', 'x-request-id': request_id})
            response = connection.getresponse()
            status = response.status
            while line := response.readline():
                received.extend(line)
                if b'CAPTURE_SAVED_PREFIX' in received:
                    prefix_seen.set()
        except (http.client.HTTPException, OSError) as failure:
            received.extend(getattr(failure, 'partial', b''))
            error = type(failure).__name__
        finally:
            connection.close()
        return {'status': status, 'raw': bytes(received), 'read_error': error}

    capacity = spool / '.capture-capacity.lock'
    before = len(fixture.captured_requests)
    with ThreadPoolExecutor(max_workers=1) as pool:
        future = pool.submit(reader)
        try:
            if not prefix_seen.wait(4) or future.done():
                raise RuntimeError('注入故障前，Client尚未收到已保存的前段')
            os.chmod(capacity, 0o600 if control == 'during' else 0o400)
            fixture.release_suffix.set()
            result = future.result(timeout=6)
            if not fixture.source_finished.wait(2):
                raise RuntimeError('受控故障來源未在期限內結束')
        finally:
            os.chmod(capacity, 0o600)
            fixture.release_suffix.set()
            fixture.stream_fault = False
    raw = result.pop('raw')
    row = {'case': 'storage-fault-during-stream', **result, 'gateway_request_id': request_id, 'source_calls': len(fixture.captured_requests) - before, 'saved_prefix_received': b'CAPTURE_SAVED_PREFIX' in raw, 'unsaved_frame_received': b'CAPTURE_UNSAVED_FRAME' in raw, 'terminal_received': (b'response.completed' if codex else b'message_stop') in raw, 'source_suffix_sent': fixture.suffix_sent, 'body_sha256': hashlib.sha256(raw).hexdigest(), 'body_bytes': len(raw)}
    rows.append(row)
    save()
    if row['status'] != 200 or row['source_calls'] != 1 or not row['saved_prefix_received'] or row['unsaved_frame_received'] or row['terminal_received'] or not row['source_suffix_sent']:
        raise RuntimeError('未保存的串流後段被送出，或保存故障結果不符')
    incomplete = capture(users['alice'][0], 'incomplete')
    partial = read_legs(incomplete, admin, 'admin-incomplete')
    for leg in ['provider_response', 'client_response']:
        if 'CAPTURE_SAVED_PREFIX' not in partial[leg] or 'CAPTURE_UNSAVED_FRAME' in partial[leg]:
            raise RuntimeError('不完整紀錄未正確保留已保存的前段')
    last = call('new-request-after-stream-fault', endpoint, users['alice'][1], method='POST', body=body(model, 'last'), calls=1)
    last_record = capture(users['alice'][0])
    if read_legs(last_record, admin, 'admin-after-stream-fault')['client_response'] != last:
        raise RuntimeError('串流保存故障後的新請求未恢復')
    verify_plaintext(runtime, database, fixture.provider_credentials | fixture.client_credentials)
    if not all(p['requests_saved_before_source'] for p in fixture.source_proofs):
        raise RuntimeError('來源呼叫前缺少已持久保存的請求')
    save()
    return rows
