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
    args = parser.parse_args()
    database = os.environ['DATABASE_URL']
    parsed = urlsplit(database)
    if os.environ.get('CLIENT_IDENTITY_ALLOW_DATABASE_MUTATION') != '1' or parsed.hostname not in ('127.0.0.1', 'localhost') or not parsed.path.startswith('/gateway_client_identity_validation_'):
        raise RuntimeError('需要本機隔離測試資料庫')
    version = subprocess.check_output(['claude', '--version'], text=True).strip()
    if not version.startswith('2.1.318 '):
        raise RuntimeError('需要Claude Code 2.1.318')
    root = Path(__file__).resolve().parent.parent
    runtime = Path(tempfile.mkdtemp(prefix='gateway-client-identity-'))
    os.chmod(runtime, 0o700)
    captures = runtime / 'captures'
    captures.mkdir()
    fixture = runpy.run_path(str(root / 'scripts/client-roundtrip-mock.py'))['FixtureHandler']
    fixture.fixture_dir = root / 'tests/fixtures/client-roundtrip'
    fixture.capture_dir = captures
    fixture.request_paths = {'claude-code': [], 'codex-cli': []}
    fixture.client_credentials = set()
    fixture.captured_requests = []
    fixture.provider_keys_by_model = {'claude-alice': 'fixture-alice-api', 'claude-bob': 'fixture-bob-api'}
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
        'ANTHROPIC_BASE_URL': f'http://127.0.0.1:{server.server_port}', 'OPENAI_BASE_URL': 'http://127.0.0.1:1',
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

        provider = create('providers', {'provider_key': 'anthropic', 'display_name': '假供應商', 'provider_type': 'anthropic', 'base_url': environment['ANTHROPIC_BASE_URL'], 'credential_ref': 'env://ANTHROPIC_API_KEY', 'supported_operations': ['anthropic_messages']})['resource']
        activate('providers', provider)
        users = {}
        for name in ('alice', 'bob', 'missing'):
            model = 'claude-' + name
            resource = create('models', {'provider_id': provider['id'], 'public_model_id': model, 'provider_model_id': model, 'supported_operations': ['anthropic_messages'], 'supports_streaming': True, 'supports_tools': True, 'supports_reasoning': True})['resource']
            activate('models', resource)
            user = create('users', {'display_name': name})['resource']
            credential = create('credentials', {'display_name': name, 'user_id': user['id']})['secret']
            fixture.client_credentials.add(credential)
            users[name] = (user['id'], credential)
            policy = create('policies', {'policy_key': name, 'revision': 1, 'user_id': user['id'], 'configuration': {'constraints': {'allowed_models': [model]}}})['resource']
            activate('policies', policy)
            if name != 'missing':
                api(f'/admin/v1/provider-api-keys/anthropic/{user["id"]}', 'PUT', {'token': fixture.provider_keys_by_model[model]}, admin)

        def client(name, model, success, label):
            before = len(fixture.request_paths['claude-code'])
            capture_start = len(fixture.captured_requests)
            denial_status = None
            if not success:
                denial_status = 403 if label == 'model-denied' else 503
                api('/v1/messages', 'POST', {'model': model, 'max_tokens': 1, 'messages': [{'role': 'user', 'content': 'fixture'}]}, users[name][1], denial_status)
            home = runtime / label
            home.mkdir()
            result = subprocess.run(['timeout', '--kill-after=5s', '30s', 'claude', '-p', '--model', model, '--max-turns', '1', 'Reply with exactly W05_CLAUDE_CODE_OK and nothing else.'], env={
                'PATH': os.environ['PATH'], 'CLAUDE_CONFIG_DIR': str(home), 'ANTHROPIC_BASE_URL': base,
                'ANTHROPIC_AUTH_TOKEN': users[name][1], 'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC': '1',
                'DISABLE_AUTOUPDATER': '1', 'DISABLE_TELEMETRY': '1',
            }, cwd=runtime, capture_output=True, text=True, timeout=40)
            after = len(fixture.request_paths['claude-code'])
            diagnostic = result.stdout + '\n' + result.stderr
            for secret in fixture.client_credentials | fixture.provider_credentials:
                diagnostic = diagnostic.replace(secret, '[credential]')
            (home / 'client-diagnostic.txt').write_text(diagnostic)
            if success:
                if result.returncode != 0 or result.stdout.strip() != 'W05_CLAUDE_CODE_OK' or after <= before:
                    raise RuntimeError(f'{label}: 原Client未完成假串流')
                observed = fixture.captured_requests[capture_start:]
                if not observed or any(capture['model'] != model or not capture['provider_credential_matches_fixture'] or not capture['client_credential_absent'] for capture in observed):
                    raise RuntimeError(f'{label}: 憑證隔離失敗')
            elif after != before or result.stdout.strip() == 'W05_CLAUDE_CODE_OK':
                raise RuntimeError(f'{label}: 應拒絕卻送出或回成功')
            checks.append({'case': label, 'passed': True, 'upstream_requests': after - before, 'client_exit': result.returncode, 'denial_status': denial_status})

        client('alice', 'claude-alice', True, 'alice-personal')
        client('bob', 'claude-bob', True, 'bob-personal')
        client('alice', 'claude-bob', False, 'model-denied')
        client('missing', 'claude-missing', False, 'missing-credential')
        alice = users['alice'][0]
        api(f'/admin/v1/provider-api-keys/anthropic/{alice}', 'DELETE', token=admin, expected=204)
        client('alice', 'claude-alice', False, 'personal-revoked')
        group = create('groups', {'display_name': '本人群組'})
        api(f'/admin/v1/groups/{group["id"]}/members/{alice}', 'POST', token=admin, expected=204)
        api(f'/admin/v1/groups/{group["id"]}/provider-credentials/anthropic/api', 'PUT', {'token': 'fixture-alice-group'}, admin, 204)
        fixture.provider_keys_by_model['claude-alice'] = 'fixture-alice-group'
        client('alice', 'claude-alice', True, 'group-fallback')
        api(f'/admin/v1/groups/{group["id"]}/members/{alice}', 'DELETE', token=admin, expected=204)
        client('alice', 'claude-alice', False, 'membership-revoked')
        (runtime / 'result.json').write_text(json.dumps({'revision': args.revision, 'client_version': version, 'checks': checks}, indent=2))
        print(f'原Client身分隔離通過：{len(checks)}項；證據：{runtime}')
    finally:
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
