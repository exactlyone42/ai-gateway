from concurrent.futures import ThreadPoolExecutor
import fcntl
import hashlib
import json
import threading
import time


def handler(base):
    class RaceFixture(base):
        gate_lock = threading.Lock()
        remaining = 0
        started = threading.Event()
        headers_sent = threading.Event()
        release_headers = threading.Event()
        release_body = threading.Event()

        @classmethod
        def arm(cls, count=1):
            with cls.gate_lock:
                cls.remaining = count
            for event in [cls.started, cls.headers_sent, cls.release_headers, cls.release_body]:
                event.clear()

        def do_POST(self):
            with self.gate_lock:
                held = type(self).remaining > 0
                if held:
                    type(self).remaining -= 1
            if not held:
                return super().do_POST()
            self._record_path()
            self._capture('codex-cli' if self.path.startswith('/v1/responses') else 'claude-code', self._json_body())
            self.started.set()
            if not self.release_headers.wait(12):
                self._send(504, 'application/json', b'{"error":"fixture header gate expired"}')
                return
            payload = (self.fixture_dir / ('openai-response.sse' if self.path.startswith('/v1/responses') else 'anthropic-response.sse')).read_bytes()
            self.send_response(200)
            self.send_header('content-type', 'text/event-stream')
            self.send_header('content-length', str(len(payload)))
            self.send_header('connection', 'close')
            self.end_headers()
            self.wfile.flush()
            self.headers_sent.set()
            if self.release_body.wait(12):
                self.wfile.write(payload)
                self.wfile.flush()
    return RaceFixture


def verify(api, create, activate, users, provider, fixture, runtime, admin, codex, quota_control):
    prefix = 'company' if codex else 'claude'
    model = prefix + '-alice'
    path = '/v1/responses' if codex else '/v1/messages'
    owner, key, credential_id = users['alice']
    provider_key = provider['provider_key']
    tenant = '00000000-0000-0000-0000-000000000001'
    api_root = runtime / 'personal' / 'api' / hashlib.sha256(provider_key.encode()).hexdigest()
    personal_lock = api_root / tenant / ('.' + owner + '.lock')
    original_source = fixture.provider_keys_by_model[model]
    source = original_source
    rows, phases = [], []
    result_lock = threading.Lock()

    def save():
        (runtime / 'access-race.json').write_text(json.dumps({'scope': 'isolated HTTP with controlled source headers/body; no native Client', 'cases': rows, 'phases': phases}, indent=2))

    def call(label, endpoint, token, expected=200, method='GET', body=None):
        before = time.monotonic()
        metadata = {}
        value = api(endpoint, method, body, token, expected=None, raw_response=True, response_metadata=metadata)
        data = endpoint == path
        complete = ('response.completed' if codex else 'message_stop') in value if data and metadata['status'] == 200 else True
        safe = all(secret not in value for secret in fixture.provider_credentials | fixture.client_credentials)
        row = {'case': label, 'status': metadata['status'], 'expected_status': expected, 'gateway_request_id': metadata['request_id'], 'started': before, 'finished': time.monotonic(), 'complete': complete, 'secrets_absent': safe}
        with result_lock:
            rows.append(row)
            save()
        if (expected is not None and row['status'] != expected) or not all([complete, safe, metadata['request_id']]):
            raise RuntimeError(f'access race rejected {label}: status={row["status"]}')
        return row

    def data(label, token, expected=200, public=model):
        body = {'model': public, 'stream': True}
        body.update({'input': 'Reply OK', 'max_output_tokens': 64} if codex else {'messages': [{'role': 'user', 'content': 'Reply OK'}], 'max_tokens': 64})
        return call(label, path, token, expected, 'POST', body)

    def source_phase(label, before, expected, **proof):
        observed = fixture.captured_requests[before:]
        matches = all(row['provider_credential_matches_fixture'] and row['client_credential_absent'] for row in observed)
        phase = {'phase': label, 'source_calls': len(observed), 'expected_source_calls': expected, 'credentials_match': matches, **proof}
        phases.append(phase)
        save()
        if len(observed) != expected or not matches:
            raise RuntimeError(f'access race rejected {label}: source_calls={len(observed)}')

    def available(lock_path):
        with lock_path.open('rb') as stream:
            try:
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return False
            fcntl.flock(stream, fcntl.LOCK_UN)
            return True

    def held(pool, label):
        fixture.arm()
        future = pool.submit(data, label, key)
        if not fixture.started.wait(5) or future.done():
            raise RuntimeError('controlled source did not hold the request')
        return future

    def headers_only(future, lock_paths):
        fixture.release_headers.set()
        if not fixture.headers_sent.wait(3):
            raise RuntimeError('source did not send response headers')
        deadline = time.monotonic() + 3
        while not all(available(p) for p in lock_paths):
            if time.monotonic() >= deadline:
                raise RuntimeError('credential lease remained after source headers')
            time.sleep(0.01)
        if future.done() or fixture.release_body.is_set():
            raise RuntimeError('old response completed before the body gate')
        return time.monotonic()

    with ThreadPoolExecutor(max_workers=3) as pool:
        try:
            start = len(fixture.captured_requests)
            data('warm-gateway-key', key)
            old = held(pool, 'admitted-before-gateway-revoke')
            current = api(f'/admin/v1/credentials/{credential_id}', token=admin)
            revoked = call('revoke-gateway-key', f'/admin/v1/credentials/{credential_id}?version={current["version"]}', admin, 200, 'DELETE')
            data('new-request-after-revoke', key, 401)
            boundary = headers_only(old, [personal_lock])
            fixture.release_body.set()
            finished = old.result(timeout=5)
            data('no-cache-resurrection-after-old-response', key, 401)
            if not revoked['finished'] < boundary < finished['finished']:
                raise RuntimeError('gateway revocation boundary is not ordered')
            source_phase('gateway-key-revoke', start, 2, revoke_before_source_headers=True, old_completed_after_revoke=True, new_requests_rejected=True)
            replacement = create('credentials', {'display_name': '競態新金鑰', 'user_id': owner})
            alternate_key = replacement['secret']
            rotation_metadata = {}
            rotation = api(f'/admin/v1/credentials/{replacement["resource"]["id"]}/rotate', 'POST', {'version': replacement['resource']['version'], 'overlap_seconds': 900}, admin, 201, response_metadata=rotation_metadata)
            key = rotation['secret']
            if key == alternate_key or rotation['resource']['user_id'] != owner:
                raise RuntimeError('Gateway key rotation changed the owner or reused the secret')
            fixture.client_credentials.update([key, alternate_key])
            phases.append({'phase': 'gateway-key-rotation', 'gateway_request_id': rotation_metadata['request_id'], 'old_credential_id': replacement['resource']['id'], 'new_credential_id': rotation['resource']['id'], 'overlap_seconds': 900, 'same_owner': True, 'expiry_not_tested': True})
            save()
            start = len(fixture.captured_requests)
            data('overlap-old-key-still-valid', alternate_key)
            data('replacement-gateway-key-works', key)
            old = held(pool, 'personal-source-before-headers')
            if available(personal_lock):
                raise RuntimeError('personal source lock not held before headers')
            target = f'/admin/v1/provider-api-keys/{provider_key}/{owner}'
            call('personal-update-busy', target, admin, 503, 'PUT', {'token': 'fixture-updated-personal-api'})
            call('personal-revoke-busy', target, admin, 503, 'DELETE')
            api(target, token=admin)
            if (api_root / tenant / owner).read_text() != source:
                raise RuntimeError('busy personal operation changed the source')
            headers_only(old, [personal_lock])
            source = 'fixture-updated-personal-api'
            fixture.provider_credentials.add(source)
            call('personal-update-after-headers', target, admin, 200, 'PUT', {'token': source})
            fixture.provider_keys_by_model[model] = source
            data('new-personal-during-old-stream', key)
            if old.done():
                raise RuntimeError('personal update did not precede old response end')
            fixture.release_body.set()
            old.result(timeout=5)
            source_phase('personal-update', start, 4, busy_preserved_original=True, updated_before_old_body_finished=True)
            call('remove-personal-to-use-group', target, admin, 204, 'DELETE')
            group = create('groups', {'display_name': '本人競態群組'})
            member = f'/admin/v1/groups/{group["id"]}/members/{owner}'
            group_target = f'/admin/v1/groups/{group["id"]}/provider-credentials/{provider_key}/api'
            call('add-group-member', member, admin, 204, 'POST')
            group_source = 'fixture-race-group-api'
            fixture.provider_credentials.add(group_source)
            call('set-group-source', group_target, admin, 204, 'PUT', {'token': group_source})
            fixture.provider_keys_by_model[model] = group_source
            start = len(fixture.captured_requests)
            old = held(pool, 'group-source-before-headers')
            group_lock = api_root / 'groups' / hashlib.sha256(provider_key.encode()).hexdigest() / tenant / ('.' + group['id'] + '.lock')
            member_lock = runtime / 'personal' / 'memberships' / tenant / ('.' + owner + '.lock')
            if available(group_lock) or available(member_lock):
                raise RuntimeError('group lease not held before headers')
            call('group-update-busy', group_target, admin, 503, 'PUT', {'token': 'fixture-updated-group-api'})
            call('member-removal-busy', member, admin, 503, 'DELETE')
            api(group_target, token=admin)
            members = api(f'/admin/v1/groups/{group["id"]}/members', token=admin)
            if owner not in {m['user_id'] for m in members['items']}:
                raise RuntimeError('busy membership operation changed membership')
            headers_only(old, [group_lock, member_lock])
            group_source = 'fixture-updated-group-api'
            fixture.provider_credentials.add(group_source)
            call('group-update-after-headers', group_target, admin, 204, 'PUT', {'token': group_source})
            fixture.provider_keys_by_model[model] = group_source
            data('new-group-during-old-stream', key)
            api(f'/admin/v1/groups/{group["id"]}/members', token=admin)
            call('member-removal-after-headers', member, admin, 204, 'DELETE')
            data('no-new-group-request-after-removal', key, 503)
            data('bob-unaffected-by-owner-removal', users['bob'][1], public=prefix + '-bob')
            call('restore-personal-source', target, admin, 200, 'PUT', {'token': source})
            fixture.provider_keys_by_model[model] = source
            data('owner-personal-after-group-removal', key)
            if old.done():
                raise RuntimeError('group changes did not precede old response end')
            fixture.release_body.set()
            old.result(timeout=5)
            source_phase('group-update-and-removal', start, 4, busy_kept_member=True, changes_before_old_body_finished=True)
            seconds = time.time() % 60
            if seconds > 40:
                time.sleep(60 - seconds + 0.05)
            quota = create('quotas', {'quota_key': 'last-request-race', 'revision': 1, 'user_id': owner, 'configuration': {'mode': 'hard', 'limits': {'requests_per_minute': 2 if quota_control else 1}}})['resource']
            activate('quotas', quota)
            start = len(fixture.captured_requests)
            window = int(time.time() // 60)
            fixture.arm(2)
            barrier = threading.Barrier(2)

            def competitor(label, candidate_key):
                barrier.wait(timeout=3)
                return data(label, candidate_key, None)

            futures = [pool.submit(competitor, label, candidate) for label, candidate in [('last-quota-a', key), ('last-quota-b', alternate_key)]]
            deadline = time.monotonic() + 5
            while not any(f.done() for f in futures) and len(fixture.captured_requests) - start < 2:
                if time.monotonic() >= deadline:
                    raise RuntimeError('quota race did not reach a bounded decision')
                time.sleep(0.01)
            fixture.release_headers.set()
            fixture.release_body.set()
            outcomes = [f.result(timeout=5) for f in futures]
            phase = {'phase': 'last-quota-decision', 'statuses': sorted(r['status'] for r in outcomes), 'source_calls': len(fixture.captured_requests) - start, 'configured_limit': 2 if quota_control else 1, 'quota_id': quota['id'], 'utc_minute': window}
            phases.append(phase)
            save()
            if phase['statuses'] != [200, 429] or phase['source_calls'] != 1 or int(time.time() // 60) != window:
                raise RuntimeError('last quota was not allocated exactly once')
            data('same-minute-still-exhausted', key, 429)
            source_phase('quota-active', start, 1, utc_minute=window)
            current = api(f'/admin/v1/quotas/{quota["id"]}', token=admin)
            call('disable-test-quota', f'/admin/v1/quotas/{quota["id"]}/lifecycle', admin, 200, 'POST', {'version': current['version'], 'state': 'disabled'})
            data('new-request-after-quota-disabled', key)
            source_phase('quota-disabled-recovery', start, 2)
        finally:
            fixture.release_headers.set()
            fixture.release_body.set()
    return rows, phases
