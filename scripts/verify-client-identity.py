import argparse
import json
import os
from pathlib import Path
import runpy
import shutil
import socket
import subprocess
import tempfile
import threading
import time
from http.server import ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from urllib.parse import urlsplit
import uuid


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--gateway-binary', required=True, type=Path)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--client', choices=('claude', 'codex'), default='claude')
    parser.add_argument('--check-tools', action='store_true')
    parser.add_argument('--control-wrong-tool-result', action='store_true')
    args = parser.parse_args()
    if args.control_wrong_tool_result and not args.check_tools:
        parser.error('wrong tool result control requires --check-tools')
    database = os.environ['DATABASE_URL']
    parsed = urlsplit(database)
    if os.environ.get('CLIENT_IDENTITY_ALLOW_DATABASE_MUTATION') != '1' or parsed.hostname not in ('127.0.0.1', 'localhost') or not parsed.path.startswith('/gateway_client_identity_validation_'):
        raise RuntimeError('需要本機隔離測試資料庫')
    version = subprocess.check_output([args.client, '--version'], text=True).strip()
    required = 'codex-cli 0.166.0' if args.client == 'codex' else '2.1.318 '
    if not version.startswith(required):
        raise RuntimeError('Client版本不符')
    codex = args.client == 'codex'
    provider_key = 'openai' if codex else 'anthropic'
    operation = 'openai_responses' if codex else 'anthropic_messages'
    model_prefix = 'company' if codex else 'claude'
    client_key = 'codex-cli' if codex else 'claude-code'
    response_path = '/v1/responses' if codex else '/v1/messages'
    expected_reply = 'W06_CODEX_OK' if codex else 'W05_CLAUDE_CODE_OK'
    root = Path(__file__).resolve().parent.parent
    runtime = Path(tempfile.mkdtemp(prefix='gateway-client-identity-'))
    os.chmod(runtime, 0o700)
    captures = runtime / 'captures'
    captures.mkdir()
    fixture = runpy.run_path(str(root / 'scripts/client-roundtrip-mock.py'))['FixtureHandler']
    if args.check_tools:
        fixture = runpy.run_path(str(root / 'scripts/client_tool_roundtrip_fixture.py'))['handler'](fixture)
    fixture.fixture_dir = root / 'tests/fixtures/client-roundtrip'
    fixture.capture_dir = captures
    fixture.request_paths = {'claude-code': [], 'codex-cli': []}
    fixture.client_credentials = set()
    fixture.captured_requests = []
    fixture.provider_keys_by_model = {f'{model_prefix}-alice': 'fixture-alice-api', f'{model_prefix}-bob': 'fixture-bob-api'}
    fixture.provider_credentials = set(fixture.provider_keys_by_model.values()) | {'fixture-alice-group'}
    server = ThreadingHTTPServer(('127.0.0.1', 0), fixture)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        gateway_port = listener.getsockname()[1]
    base = f'http://127.0.0.1:{gateway_port}'
    binary = runtime / 'gateway'
    shutil.copy2(args.gateway_binary, binary)
    bootstrap = 'isolated-client-identity-bootstrap-token-32-bytes'
    environment = {
        'PATH': os.environ['PATH'], 'TOKIO_WORKER_THREADS': '2',
        'DATABASE_URL': database, 'DATABASE_MAX_CONNECTIONS': '5', 'ADMIN_DATABASE_MAX_CONNECTIONS': '2',
        'CREDENTIAL_ENVIRONMENT': 'dev', 'CREDENTIAL_HASH_KEY': 'isolated-client-identity-hash-key-32-bytes',
        'ADMIN_BOOTSTRAP_TOKEN': bootstrap, 'ADMIN_BOOTSTRAP_TENANT_ID': '00000000-0000-0000-0000-000000000001',
        'CONTENT_CAPTURE_ENABLED': 'false', 'LISTEN_ADDRESS': f'127.0.0.1:{gateway_port}',
        'ANTHROPIC_BASE_URL': 'http://127.0.0.1:1' if codex else f'http://127.0.0.1:{server.server_port}',
        'OPENAI_BASE_URL': f'http://127.0.0.1:{server.server_port}' if codex else 'http://127.0.0.1:1',
        'ANTHROPIC_PERSONAL_TOKEN_DIR': str(runtime / 'personal'), 'LEDGER_SPOOL_DIR': str(runtime / 'ledger'),
    }
    checks = []

    def api(path, method='GET', body=None, token=None, expected=200):
        headers = {'x-request-id': str(uuid.uuid4())}
        if token:
            headers['authorization'] = f'Bearer {token}'
        data = None if body is None else json.dumps(body).encode()
        if data is not None:
            headers['content-type'] = 'application/json'
        try:
            with urlopen(Request(base + path, data=data, headers=headers, method=method), timeout=8) as response:
                status, raw = response.status, response.read()
        except HTTPError as error:
            status, raw = error.code, error.read()
        if status != expected:
            raise RuntimeError(f'{method} {path}: {status}, expected {expected}')
        return json.loads(raw) if raw else None

    log = (runtime / 'gateway.log').open('wb')
    gateway = subprocess.Popen(['timeout', '--kill-after=5s', '240s', str(binary)], cwd=root, env=environment, stdout=log, stderr=log)
    try:
        time.sleep(2)
        if gateway.poll() is not None:
            raise RuntimeError(f'Gateway啟動失敗，紀錄位於{runtime}')
        if api('/health')['revision'] != args.revision:
            raise RuntimeError('Gateway版本不符')
        login = api('/admin/v1/bootstrap', 'POST', {'display_name': '隔離管理者', 'credential_name': '隔離登入'}, bootstrap, 201)['login_token']
        admin = api('/admin/v1/sessions', 'POST', token=login, expected=201)['access_token']

        def create(kind, body):
            return api('/admin/v1/' + kind, 'POST', body, admin, 201)

        def activate(kind, resource):
            api(f'/admin/v1/{kind}/{resource["id"]}/lifecycle', 'POST', {'version': resource['version'], 'state': 'active'}, admin)

        provider = create('providers', {'provider_key': provider_key, 'display_name': '假供應商', 'provider_type': provider_key, 'base_url': environment['OPENAI_BASE_URL' if codex else 'ANTHROPIC_BASE_URL'], 'credential_ref': 'env://OPENAI_API_KEY' if codex else 'env://ANTHROPIC_API_KEY', 'supported_operations': [operation]})['resource']
        activate('providers', provider)
        users = {}
        for name in ('alice', 'bob', 'missing'):
            model = model_prefix + '-' + name
            resource = create('models', {'provider_id': provider['id'], 'public_model_id': model, 'provider_model_id': model, 'supported_operations': [operation], 'supports_streaming': True, 'supports_tools': True, 'supports_reasoning': True})['resource']
            activate('models', resource)
            user = create('users', {'display_name': name})['resource']
            credential = create('credentials', {'display_name': name, 'user_id': user['id']})['secret']
            fixture.client_credentials.add(credential)
            users[name] = (user['id'], credential)
            policy = create('policies', {'policy_key': name, 'revision': 1, 'user_id': user['id'], 'configuration': {'constraints': {'allowed_models': [model]}}})['resource']
            activate('policies', policy)
            if name != 'missing':
                api(f'/admin/v1/provider-api-keys/{provider_key}/{user["id"]}', 'PUT', {'token': fixture.provider_keys_by_model[model]}, admin)

        def client(name, model, success, label, tool_prompt=None):
            before = len(fixture.request_paths[client_key])
            capture_start = len(fixture.captured_requests)
            denial_status = None
            if not success:
                denial_status = 403 if label == 'model-denied' else 503
                body = {'model': model, 'input': 'fixture', 'max_output_tokens': 1} if codex else {'model': model, 'max_tokens': 1, 'messages': [{'role': 'user', 'content': 'fixture'}]}
                api(response_path, 'POST', body, users[name][1], denial_status)
            home = runtime / label
            home.mkdir()
            prompt = tool_prompt or f'Reply with exactly {expected_reply} and nothing else.'
            if codex:
                (home / 'config.toml').write_text(f'model = "{model}"\nmodel_provider = "fixture"\nmodel_reasoning_effort = "low"\n[model_providers.fixture]\nname = "Isolated Gateway"\nbase_url = "{base}/v1"\nenv_key = "GATEWAY_TEST_KEY"\nwire_api = "responses"\nrequires_openai_auth = false\n')
                command = ['codex', 'exec', '--sandbox', 'read-only', '--skip-git-repo-check', prompt]
                client_environment = {'PATH': os.environ['PATH'], 'CODEX_HOME': str(home), 'GATEWAY_TEST_KEY': users[name][1]}
            else:
                command = ['claude', '-p', '--model', model, '--max-turns', '3' if tool_prompt else '1', *(['--allowedTools=Read'] if tool_prompt else []), prompt]
                client_environment = {
                    'PATH': os.environ['PATH'], 'CLAUDE_CONFIG_DIR': str(home), 'ANTHROPIC_BASE_URL': base,
                    'ANTHROPIC_AUTH_TOKEN': users[name][1], 'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC': '1',
                    'DISABLE_AUTOUPDATER': '1', 'DISABLE_TELEMETRY': '1',
                }
            result = subprocess.run(['timeout', '--kill-after=5s', '30s', *command], env=client_environment, cwd=runtime, capture_output=True, text=True, timeout=40)
            after = len(fixture.request_paths[client_key])
            diagnostic = result.stdout + '\n' + result.stderr
            for secret in fixture.client_credentials | fixture.provider_credentials:
                diagnostic = diagnostic.replace(secret, '[credential]')
            (home / 'client-diagnostic.txt').write_text(diagnostic)
            if success:
                if result.returncode != 0 or result.stdout.strip() != expected_reply or after <= before:
                    raise RuntimeError(f'{label}: 原Client未完成假串流')
                observed = fixture.captured_requests[capture_start:]
                if not observed or any(capture['model'] != model or not capture['provider_credential_matches_fixture'] or not capture['client_credential_absent'] for capture in observed):
                    raise RuntimeError(f'{label}: 憑證隔離失敗')
            elif after != before or result.stdout.strip() == expected_reply:
                raise RuntimeError(f'{label}: 應拒絕卻送出或回成功')
            checks.append({'case': label, 'passed': True, 'upstream_requests': after - before, 'client_exit': result.returncode, 'denial_status': denial_status})

        if not args.check_tools:
            client('alice', f'{model_prefix}-alice', True, 'alice-personal')
            client('bob', f'{model_prefix}-bob', True, 'bob-personal')
        if args.check_tools:
            fixture.context_marker = 'context-' + uuid.uuid4().hex
            fixture.expected_value = 'tool-value-' + uuid.uuid4().hex
            fixture.nonce_path = str(runtime / 'tool-input.txt')
            Path(fixture.nonce_path).write_text(fixture.expected_value)
            if args.control_wrong_tool_result:
                fixture.expected_value = 'wrong-tool-value-' + uuid.uuid4().hex
            fixture.tool_mode = True
            client('alice', f'{model_prefix}-alice', True, 'tool-roundtrip', f'Read {fixture.nonce_path} using your local tool. Preserve {fixture.context_marker}. Then reply with exactly {expected_reply} and nothing else.')
            fixture.tool_mode = False
            if fixture.tool_requests != 2 or not fixture.tool_result_received:
                raise RuntimeError('原Client沒有完成工具要求、結果與上下文往返')
            checks[-1].update({'tool_requests': fixture.tool_requests, 'real_tool_result': fixture.tool_result_received, 'original_context_preserved': fixture.context_preserved, 'tool_name': fixture.tool_name})
            (runtime / 'result.json').write_text(json.dumps({'revision': args.revision, 'client_version': version, 'checks': checks}, indent=2))
            print(f'原Client工具往返通過；證據：{runtime}')
            return
        client('alice', f'{model_prefix}-bob', False, 'model-denied')
        client('missing', f'{model_prefix}-missing', False, 'missing-credential')
        alice = users['alice'][0]
        api(f'/admin/v1/provider-api-keys/{provider_key}/{alice}', 'DELETE', token=admin, expected=204)
        client('alice', f'{model_prefix}-alice', False, 'personal-revoked')
        group = create('groups', {'display_name': '本人群組'})
        api(f'/admin/v1/groups/{group["id"]}/members/{alice}', 'POST', token=admin, expected=204)
        api(f'/admin/v1/groups/{group["id"]}/provider-credentials/{provider_key}/api', 'PUT', {'token': 'fixture-alice-group'}, admin, 204)
        fixture.provider_keys_by_model[f'{model_prefix}-alice'] = 'fixture-alice-group'
        client('alice', f'{model_prefix}-alice', True, 'group-fallback')
        api(f'/admin/v1/groups/{group["id"]}/members/{alice}', 'DELETE', token=admin, expected=204)
        client('alice', f'{model_prefix}-alice', False, 'membership-revoked')
        (runtime / 'result.json').write_text(json.dumps({'revision': args.revision, 'client_version': version, 'checks': checks}, indent=2))
        print(f'原Client身分隔離通過：{len(checks)}項；證據：{runtime}')
    finally:
        if args.check_tools:
            (runtime / 'tool-result.json').write_text(json.dumps({'tool_requests': fixture.tool_requests, 'real_tool_result': fixture.tool_result_received, 'original_context_preserved': fixture.context_preserved, 'rejection': fixture.tool_rejection}, indent=2))
            print(f'工具測試證據：{runtime}')
        gateway.terminate()
        try:
            gateway.wait(timeout=7)
        except subprocess.TimeoutExpired:
            gateway.kill()
            gateway.wait()
        log.close()
        server.shutdown()
        server.server_close()
        thread.join(timeout=3)


if __name__ == '__main__':
    main()
