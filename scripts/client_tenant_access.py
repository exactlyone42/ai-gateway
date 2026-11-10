import base64
import json


def handler(base):
    class TenantFixture(base):
        def do_POST(self):
            if any(self.headers.get(name) == value for name, value in [('authorization', 'Bearer fixture-invalid-personal-api'), ('x-api-key', 'fixture-invalid-personal-api')]):
                self._record_path()
                self._capture('codex-cli' if self.path.startswith('/v1/responses') else 'claude-code', self._json_body())
                self._send(401, 'application/json', b'{"error":{"type":"authentication_error","message":"credential rejected"}}')
                return
            super().do_POST()
    return TenantFixture


def verify(api, create, activate, users, provider, fixture, runtime, admin, other_admin, codex, admin_control):
    prefix = 'company' if codex else 'claude'
    operation = 'openai_responses' if codex else 'anthropic_messages'
    model = f'{prefix}-alice'
    other_model = f'{prefix}-tenant-b'
    provider_key = provider['provider_key']
    alice = users['alice'][0]
    alice_key = fixture.provider_keys_by_model[model]
    other_key = 'fixture-tenant-b-api'
    group_key = 'fixture-alice-group'
    fixture.provider_credentials.update([other_key, 'fixture-invalid-personal-api', group_key])
    fixture.client_credentials.update([admin, other_admin])
    other_provider = create('providers', {'provider_key': provider_key, 'display_name': '租戶B來源', 'provider_type': provider['provider_type'], 'base_url': provider['base_url'], 'credential_ref': provider['credential_ref'], 'supported_operations': [operation]}, other_admin)['resource']
    activate('providers', other_provider, other_admin)
    for public in [model, other_model]:
        resource = create('models', {'provider_id': other_provider['id'], 'public_model_id': public, 'provider_model_id': public, 'supported_operations': [operation], 'supports_streaming': True}, other_admin)['resource']
        activate('models', resource, other_admin)
    other_user = create('users', {'display_name': 'alice'}, other_admin)['resource']
    other_credential = create('credentials', {'display_name': '租戶B金鑰', 'user_id': other_user['id']}, other_admin)
    other_gateway_key = other_credential['secret']
    fixture.client_credentials.add(other_gateway_key)
    policy = create('policies', {'policy_key': 'tenant-b', 'revision': 1, 'user_id': other_user['id'], 'configuration': {'constraints': {'allowed_models': [model, other_model]}}}, other_admin)['resource']
    activate('policies', policy, other_admin)
    policy = create('policies', {'policy_key': 'tenant-a-cross-model', 'revision': 2, 'user_id': alice, 'configuration': {'constraints': {'allowed_models': [model, other_model]}}})['resource']
    activate('policies', policy)
    api(f'/admin/v1/provider-api-keys/{provider_key}/{other_user["id"]}', 'PUT', {'token': other_key}, other_admin)
    group = create('groups', {'display_name': '租戶A本人群組'})
    other_group = create('groups', {'display_name': '租戶B本人群組'}, other_admin)
    api(f'/admin/v1/groups/{group["id"]}/members/{alice}', 'POST', token=admin, expected=204)
    api(f'/admin/v1/groups/{group["id"]}/provider-credentials/{provider_key}/api', 'PUT', {'token': group_key}, admin, 204)
    api(f'/admin/v1/groups/{other_group["id"]}/provider-credentials/{provider_key}/api', 'PUT', {'token': other_key}, other_admin, 204)
    rows = []

    def check(label, path, token, expected, method='GET', body=None, calls=0, source=None, ids=None):
        data_plane = path in ('/v1/messages', '/v1/responses')
        if source is not None:
            fixture.provider_keys_by_model[body['model']] = source
        before = len(fixture.captured_requests)
        metadata = {}
        value = api(path, method, body, token, expected=None, raw_response=data_plane, response_metadata=metadata)
        observed = fixture.captured_requests[before:]
        matches = all(item['provider_credential_matches_fixture'] and item['client_credential_absent'] and item['model'] == body['model'] for item in observed)
        secrets_absent = all(secret not in json.dumps(value) for secret in fixture.provider_credentials | fixture.client_credentials)
        complete = (('response.completed' if codex else 'message_stop') in value) if data_plane and expected == 200 else True
        scoped = {item['id'] for item in value['items']} == ids if ids is not None else True
        row = {'case': label, 'status': metadata['status'], 'expected_status': expected, 'upstream_calls': len(observed), 'expected_upstream_calls': calls, 'credentials_match': matches, 'secrets_absent': secrets_absent, 'complete': complete, 'scope_matches': scoped, 'gateway_request_id': metadata['request_id']}
        rows.append(row)
        (runtime / 'tenant-access.json').write_text(json.dumps({'scope': 'two isolated tenant contexts and real API sessions, controlled provider credentials, no native Client', 'cases': rows}, indent=2))
        if row['status'] != expected or row['upstream_calls'] != calls or not all([matches, secrets_absent, complete, scoped, metadata['request_id']]):
            raise RuntimeError(f'tenant access rejected {label}: status={row["status"]}, upstream={len(observed)}')
        return value

    path = '/v1/responses' if codex else '/v1/messages'

    def inference(label, token, expected, source=None, public=model, calls=0):
        body = {'model': public, 'stream': True}
        body.update({'input': 'Reply OK', 'max_output_tokens': 64} if codex else {'messages': [{'role': 'user', 'content': 'Reply OK'}], 'max_tokens': 64})
        return check(label, path, token, expected, 'POST', body, calls, source)

    inference('tenant-a-common-model', users['alice'][1], 200, alice_key, calls=1)
    inference('tenant-b-same-model-different-source', other_gateway_key, 200, other_key, calls=1)
    check('tenant-a-user-list', '/admin/v1/users', admin, 200, ids={user[0] for user in users.values()})
    check('tenant-b-user-list', '/admin/v1/users', other_admin, 200, ids={other_user['id']})
    for kind, resource in [('users', other_user), ('credentials', other_credential['resource']), ('providers', other_provider)]:
        check('tenant-a-cannot-read-b-' + kind, f'/admin/v1/{kind}/{resource["id"]}', admin, 404)
    check('tenant-b-cannot-read-a-user', f'/admin/v1/users/{alice}', other_admin, 404)
    other_status_path = f'/admin/v1/groups/{other_group["id"]}/provider-credentials/{provider_key}/api'
    before = api(other_status_path, token=other_admin)
    for method in ['GET', 'PUT', 'DELETE']:
        check('tenant-a-cannot-' + method.lower() + '-b-group-source', other_status_path, admin, 404, method, {'token': 'fixture-attack'} if method == 'PUT' else None)
    check('tenant-a-cannot-bind-b-user', f'/admin/v1/groups/{group["id"]}/members/{other_user["id"]}', admin, 404, 'POST')
    check('tenant-a-cannot-replace-b-personal', f'/admin/v1/provider-api-keys/{provider_key}/{other_user["id"]}', admin, 404, 'PUT', {'token': 'fixture-attack'})
    if api(other_status_path, token=other_admin) != before:
        raise RuntimeError('foreign source state changed')
    inference('tenant-a-cannot-use-b-only-model', users['alice'][1], 403, public=other_model)
    inference('tenant-b-only-model-still-usable', other_gateway_key, 200, other_key, other_model, 1)
    status_path = f'/admin/v1/groups/{group["id"]}/provider-credentials/{provider_key}/api'
    check('admin-can-read-source-state', status_path, admin, 200)
    for role in ['operator', 'viewer']:
        account = create('admin-accounts', {'display_name': role, 'credential_name': role, 'role': role})
        session = api('/admin/v1/sessions', 'POST', token=account['login_token'], expected=201)['access_token']
        fixture.client_credentials.update([account['login_token'], session])
        check(role + '-can-read-own-users', '/admin/v1/users', session, 200, ids={user[0] for user in users.values()})
        for method in ['GET', 'PUT', 'DELETE']:
            check(role + '-cannot-' + method.lower() + '-source', status_path, admin if admin_control and role == 'viewer' and method == 'GET' else session, 403, method, {'token': 'fixture-attack'} if method == 'PUT' else None)
        check(role + '-cannot-change-membership', f'/admin/v1/groups/{group["id"]}/members/{users["bob"][0]}', session, 403, 'POST')
        check(role + '-group-write-permission', '/admin/v1/groups', session, 201 if role == 'operator' else 403, 'POST', {'display_name': role + '群組'})
    check('gateway-key-is-not-admin-session', '/admin/v1/users', users['alice'][1], 401)
    original_key = users['alice'][1]
    parts = original_key.split('_', 3)
    parts[-1] = ('A' if parts[-1][0] != 'A' else 'B') + parts[-1][1:]
    invalid_key = '_'.join(parts)
    nonce = base64.urlsafe_b64decode(parts[-1] + '=')
    if invalid_key == original_key or len(nonce) != 32 or base64.urlsafe_b64encode(nonce).decode().rstrip('=') != parts[-1]:
        raise RuntimeError('invalid Gateway key control did not preserve its format')
    fixture.client_credentials.add(invalid_key)
    inference('altered-gateway-key', invalid_key, 401)
    inference('admin-session-is-not-gateway-key', admin, 401)
    api(f'/admin/v1/provider-api-keys/{provider_key}/{alice}', 'PUT', {'token': 'fixture-invalid-personal-api'}, admin)
    inference('invalid-personal-does-not-fall-back', users['alice'][1], 401, 'fixture-invalid-personal-api', calls=1)
    api(f'/admin/v1/provider-api-keys/{provider_key}/{alice}', 'PUT', {'token': alice_key}, admin)
    inference('personal-restored-after-401', users['alice'][1], 200, alice_key, calls=1)
    api(f'/admin/v1/provider-api-keys/{provider_key}/{alice}', 'DELETE', token=admin, expected=204)
    api(f'/admin/v1/groups/{group["id"]}/members/{alice}', 'DELETE', token=admin, expected=204)
    inference('missing-a-does-not-use-b-or-tenant', users['alice'][1], 503)
    inference('tenant-b-still-usable', other_gateway_key, 200, other_key, calls=1)
    api(f'/admin/v1/provider-api-keys/{provider_key}/{alice}', 'PUT', {'token': alice_key}, admin)
    inference('tenant-a-still-usable', users['alice'][1], 200, alice_key, calls=1)
    return rows
