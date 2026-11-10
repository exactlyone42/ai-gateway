import hashlib
import json
import uuid
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def verify(base, api, create, activate, users, provider, fixture, runtime, admin, codex, foreign_control=False):
    provider_key = provider['provider_key']
    tenant = '00000000-0000-0000-0000-000000000001'
    prefix = 'company' if codex else 'claude'
    path = '/v1/responses' if codex else '/v1/messages'
    personal_root = runtime / 'personal' / 'api' / hashlib.sha256(provider_key.encode()).hexdigest()
    alice_file = personal_root / tenant / users['alice'][0]
    alice_key = fixture.provider_keys_by_model[f'{prefix}-alice']
    group_key = 'fixture-alice-group'
    rows = []

    def request(label, user, expected_status, source, model=None, credential=None):
        model = model or f'{prefix}-{user}'
        before = len(fixture.captured_requests)
        body = {'model': model, 'stream': True}
        body.update({'input': 'Reply OK', 'max_output_tokens': 64} if codex else {'messages': [{'role': 'user', 'content': 'Reply OK'}], 'max_tokens': 64})
        headers = {'authorization': 'Bearer ' + (credential or users[user][1]), 'content-type': 'application/json'}
        try:
            response = urlopen(Request(base + path, data=json.dumps(body).encode(), headers=headers), timeout=8)
        except HTTPError as error:
            response = error
        with response:
            raw = response.read()
            status = response.status
            request_id = response.headers.get('x-request-id')
        observed = fixture.captured_requests[before:]
        expected_calls = int(expected_status == 200)
        credentials_match = all(item['provider_credential_matches_fixture'] and item['client_credential_absent'] and item['model'] == model for item in observed)
        secrets_absent = all(secret.encode() not in raw for secret in fixture.provider_credentials | fixture.client_credentials)
        completed = (b'response.completed' if codex else b'message_stop') in raw if expected_status == 200 else True
        row = {'case': label, 'user_id': users[user][0], 'model': model, 'expected_status': expected_status, 'status': status, 'expected_upstream_calls': expected_calls, 'upstream_calls': len(observed), 'expected_source': source, 'credentials_match': credentials_match, 'secrets_absent': secrets_absent, 'completed': completed, 'gateway_request_id': request_id}
        rows.append(row)
        (runtime / 'access-matrix.json').write_text(json.dumps({'scope': 'isolated HTTP with synthetic credentials; no native Client or real account', 'cases': rows}, indent=2))
        if status != expected_status or len(observed) != expected_calls or not all((credentials_match, secrets_absent, completed, request_id)):
            raise RuntimeError(f'access matrix rejected {label}: status={status}, upstream={len(observed)}, credentials_match={credentials_match}')

    def member(group, method):
        api(f'/admin/v1/groups/{group["id"]}/members/{users["alice"][0]}', method, token=admin, expected=204)

    def group_credential(group, token):
        api(f'/admin/v1/groups/{group["id"]}/provider-credentials/{provider_key}/api', 'PUT', {'token': token}, admin, 204)

    def personal(token):
        api(f'/admin/v1/provider-api-keys/{provider_key}/{users["alice"][0]}', 'PUT' if token else 'DELETE', {'token': token} if token else None, admin, 200 if token else 204)

    def disable(kind, resource):
        current = api(f'/admin/v1/{kind}/{resource["id"]}', token=admin)
        api(f'/admin/v1/{kind}/{resource["id"]}/lifecycle', 'POST', {'version': current['version'], 'state': 'disabled'}, admin)

    if foreign_control:
        alice_file.write_text(fixture.provider_keys_by_model[f'{prefix}-bob'])
    request('alice-personal', 'alice', 200, 'alice-personal')
    request('bob-personal', 'bob', 200, 'bob-personal')
    request('alice-cannot-request-bob-model', 'alice', 403, 'none', f'{prefix}-bob')
    request('missing-never-uses-tenant-or-other-user', 'missing', 503, 'none')
    spoofed = users['alice'][1].replace(uuid.UUID(tenant).hex, uuid.uuid4().hex, 1)
    if spoofed == users['alice'][1]:
        raise RuntimeError('tenant control did not change the credential')
    request('tenant-key-spoof', 'alice', 401, 'none', credential=spoofed)
    group = create('groups', {'display_name': '本人來源矩陣'})
    member(group, 'POST')
    group_credential(group, group_key)
    request('personal-before-group', 'alice', 200, 'alice-personal')
    original = alice_file.read_bytes()
    alice_file.write_bytes(b'invalid\ncredential')
    try:
        request('corrupt-personal-never-falls-back', 'alice', 503, 'none')
    finally:
        alice_file.write_bytes(original)
    request('personal-restored', 'alice', 200, 'alice-personal')
    personal(None)
    fixture.provider_keys_by_model[f'{prefix}-alice'] = group_key
    request('unique-group', 'alice', 200, 'alice-group')
    group_file = personal_root / 'groups' / hashlib.sha256(provider_key.encode()).hexdigest() / tenant / group['id']
    original = group_file.read_bytes()
    group_file.write_text('fixture-uncommitted-group')
    try:
        request('uncommitted-group-digest', 'alice', 503, 'none')
    finally:
        group_file.write_bytes(original)
    request('group-restored', 'alice', 200, 'alice-group')
    second_key = 'fixture-alice-group-second'
    fixture.provider_credentials.add(second_key)
    second = create('groups', {'display_name': '本人第二來源矩陣'})
    member(second, 'POST')
    group_credential(second, second_key)
    request('ambiguous-groups', 'alice', 503, 'none')
    personal(alice_key)
    fixture.provider_keys_by_model[f'{prefix}-alice'] = alice_key
    request('personal-before-ambiguous-groups', 'alice', 200, 'alice-personal')
    member(second, 'DELETE')
    personal(None)
    fixture.provider_keys_by_model[f'{prefix}-alice'] = group_key
    member(group, 'DELETE')
    request('membership-revoked', 'alice', 503, 'none')
    member(group, 'POST')
    request('membership-restored', 'alice', 200, 'alice-group')
    policy = create('policies', {'policy_key': 'group-hard-matrix', 'revision': 1, 'group_id': group['id'], 'configuration': {'constraints': {'allowed_models': [f'{prefix}-bob']}}})['resource']
    activate('policies', policy)
    request('personal-cannot-relax-group-model-limit', 'alice', 403, 'none')
    request('group-policy-does-not-affect-bob', 'bob', 200, 'bob-personal')
    disable('policies', policy)
    request('group-policy-disabled', 'alice', 200, 'alice-group')
    quota = create('quotas', {'quota_key': 'group-zero-matrix', 'revision': 1, 'group_id': group['id'], 'configuration': {'mode': 'hard', 'limits': {'requests_per_minute': 0}}})['resource']
    activate('quotas', quota)
    request('group-quota-zero', 'alice', 429, 'none')
    request('group-quota-does-not-affect-bob', 'bob', 200, 'bob-personal')
    disable('quotas', quota)
    request('quota-disabled', 'alice', 200, 'alice-group')
    current = api(f'/admin/v1/credentials/{users["alice"][2]}', token=admin)
    api(f'/admin/v1/credentials/{current["id"]}?version={current["version"]}', 'DELETE', token=admin)
    request('gateway-key-revoked-after-warm-use', 'alice', 401, 'none')
    request('bob-still-usable-after-alice-revoked', 'bob', 200, 'bob-personal')
    return rows
