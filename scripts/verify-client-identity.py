import argparse
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import signal
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
    parser.add_argument('--check-codex-held', action='store_true')
    parser.add_argument('--check-local', action='store_true')
    parser.add_argument('--check-access-matrix', action='store_true')
    parser.add_argument('--check-tenant-access', action='store_true')
    parser.add_argument('--control-tenant-admin-as-viewer', action='store_true')
    parser.add_argument('--control-access-foreign-source', action='store_true')
    parser.add_argument('--check-owned-tools', action='store_true')
    parser.add_argument('--check-owned-compact', action='store_true')
    parser.add_argument('--codex-auth-file', type=Path)
    parser.add_argument('--source-model')
    parser.add_argument('--check-compact', action='store_true')
    parser.add_argument('--claude-auto-compact', action='store_true')
    parser.add_argument('--control-omit-summary', action='store_true')
    parser.add_argument('--compact-failure', choices=('summary', 'context-overflow'))
    parser.add_argument('--control-compact-success', action='store_true')
    parser.add_argument('--check-tools', action='store_true')
    parser.add_argument('--control-wrong-tool-result', action='store_true')
    parser.add_argument('--check-cancel', action='store_true')
    parser.add_argument('--check-slow-reader', action='store_true')
    parser.add_argument('--control-drain-reader', action='store_true')
    parser.add_argument('--check-long-stream', action='store_true')
    parser.add_argument('--check-capture', action='store_true')
    parser.add_argument('--check-error', action='store_true')
    parser.add_argument('--check-auth-recovery', action='store_true')
    parser.add_argument('--check-transient-recovery', choices=('rate-limit', 'unavailable', 'timeout'))
    parser.add_argument('--control-timeout-not-enforced', action='store_true')
    parser.add_argument('--check-stream-failure', action='store_true')
    parser.add_argument('--control-error-as-success', action='store_true')
    parser.add_argument('--control-force-client-kill', action='store_true')
    args = parser.parse_args()
    live_source = args.check_codex_held or args.check_local
    if args.check_access_matrix and any((args.check_tenant_access, live_source, args.check_capture, args.check_tools, args.check_cancel, args.check_error, args.check_compact, args.check_long_stream, args.check_owned_tools, args.check_owned_compact)):
        parser.error('權限矩陣與原Client及其他模式分開驗收')
    if args.check_tenant_access and any((live_source, args.check_capture, args.check_tools, args.check_cancel, args.check_error, args.check_compact, args.check_long_stream, args.check_owned_tools, args.check_owned_compact)):
        parser.error('租戶角色矩陣與原Client及其他模式分開驗收')
    if args.control_tenant_admin_as_viewer and not args.check_tenant_access:
        parser.error('角色反例僅供租戶角色矩陣')
    if args.control_access_foreign_source and not args.check_access_matrix:
        parser.error('錯用他人來源反例僅供權限矩陣')
    owned_cancel = args.check_local and args.check_cancel
    recovery_mode = args.check_auth_recovery or args.check_stream_failure or bool(args.check_transient_recovery)
    interrupted_mode = args.check_stream_failure or args.check_transient_recovery == 'timeout'
    error_status = 200 if interrupted_mode else 429 if args.check_transient_recovery == 'rate-limit' else 503 if args.check_transient_recovery == 'unavailable' else 401 if args.check_auth_recovery else 400
    if args.control_timeout_not_enforced and args.check_transient_recovery != 'timeout':
        parser.error('延長等待反例僅供串流閒置逾時驗收')
    cancel_recovery = owned_cancel or args.check_slow_reader
    if args.check_slow_reader and (not (args.check_cancel and args.check_capture) or any((live_source, args.check_tools, args.check_error, args.check_compact, args.check_long_stream, args.control_force_client_kill))):
        parser.error('慢讀必須搭配取消與保存，且與其他模式分開')
    if args.control_drain_reader and not args.check_slow_reader:
        parser.error('快讀反例僅供慢讀驗收')
    if args.check_long_stream and (not args.check_capture or any((args.check_local, args.check_codex_held, args.check_tools, args.check_cancel, args.check_error, args.check_compact))):
        parser.error('長串流必須搭配保存驗收，且與其他模式分開')
    if recovery_mode and not (args.check_error and args.check_capture):
        parser.error('認證或串流恢復驗收必須搭配 --check-error 與 --check-capture')
    if sum(map(bool, (args.check_auth_recovery, args.check_stream_failure, args.check_transient_recovery))) > 1:
        parser.error('認證拒絕與串流中斷必須分開驗收')
    if args.check_local and (args.check_codex_held or not args.source_model or any((args.check_compact, args.check_owned_compact, args.check_tools, args.check_error))):
        parser.error('local source requires a source model separately from fixture and subscription modes')
    if owned_cancel and (not args.check_capture or args.check_owned_tools):
        parser.error('實際模型取消必須驗保存，且與工具模式分開')
    if args.check_owned_compact and (not (args.check_codex_held and args.check_capture) or args.check_owned_tools):
        parser.error('owned compact requires owned source and capture, separately from tools')
    if args.check_owned_tools and not (live_source and args.check_capture):
        parser.error('owned tools require owned source and capture')
    if args.claude_auto_compact and (not args.check_compact or args.client != 'claude'):
        parser.error('Claude automatic compaction requires Claude compact mode')
    if args.client == 'claude' and args.compact_failure and not args.claude_auto_compact:
        parser.error('Claude compact failure requires reactive compact mode')
    if args.control_compact_success and not args.compact_failure:
        parser.error('compact success control requires compact failure mode')
    if args.compact_failure and (not args.check_compact or args.control_omit_summary):
        parser.error('compact failure requires compact mode separately from summary control')
    if args.control_omit_summary and not args.check_compact:
        parser.error('summary control requires compact mode')
    if args.check_compact and any((args.check_tools, args.check_cancel, args.check_error, args.check_codex_held, args.check_capture)):
        parser.error('compact validation requires codex separately from other modes')
    if args.control_wrong_tool_result and not args.check_tools:
        parser.error('wrong tool result control requires --check-tools')
    if sum((args.check_cancel, args.check_tools, args.check_error)) > 1:
        parser.error('cancel, tool and error validation must run separately')
    if args.control_force_client_kill and not args.check_cancel:
        parser.error('forced kill control requires --check-cancel')
    if args.control_error_as_success and not args.check_error:
        parser.error('error success control requires --check-error')
    if args.check_capture and not (args.check_tools or args.check_cancel or args.check_error or args.check_long_stream or live_source):
        parser.error('capture validation requires tools, cancel or error')
    if args.check_codex_held:
        if args.client != 'codex' or not args.codex_auth_file or not args.source_model or any((args.check_tools, args.check_cancel, args.check_error)):
            parser.error('owned Codex source requires codex, auth file and source model, separately from fixture modes')
        source_bytes = args.codex_auth_file.read_bytes()
        source_auth = json.loads(source_bytes)
        if source_auth.get('auth_mode') != 'chatgpt' or not source_auth.get('tokens', {}).get('account_id'):
            parser.error('owned Codex source requires a ChatGPT login')
    elif args.codex_auth_file or (args.source_model and not args.check_local):
        parser.error('auth file and source model require --check-codex-held')
    database = os.environ['DATABASE_URL']
    parsed = urlsplit(database)
    if os.environ.get('CLIENT_IDENTITY_ALLOW_DATABASE_MUTATION') != '1' or parsed.hostname not in ('127.0.0.1', 'localhost') or not parsed.path.startswith('/gateway_client_identity_validation_'):
        raise RuntimeError('需要本機隔離測試資料庫')
    local_capabilities = []
    if args.check_local:
        request = Request('http://127.0.0.1:11434/api/show', data=json.dumps({'model': args.source_model}).encode(), headers={'content-type': 'application/json'})
        with urlopen(request, timeout=8) as response:
            local_capabilities = json.load(response).get('capabilities', [])
        if 'completion' not in local_capabilities or (args.check_owned_tools and 'tools' not in local_capabilities):
            raise RuntimeError('本機模型未宣告所需的文字或工具能力')
    version = subprocess.check_output([args.client, '--version'], text=True).strip()
    required = 'codex-cli 0.166.0' if args.client == 'codex' else '2.1.319 '
    if not version.startswith(required):
        raise RuntimeError('Client版本不符')
    codex = args.client == 'codex'
    provider_key = 'local-ollama' if args.check_local else 'codex-subscription' if args.check_codex_held else ('openai' if codex else 'anthropic')
    operation = 'openai_responses' if codex else 'anthropic_messages'
    model_prefix = 'company' if codex else 'claude'
    client_key = 'codex-cli' if codex else 'claude-code'
    response_path = '/v1/responses' if codex else '/v1/messages'
    expected_reply = 'W06_CODEX_OK' if codex else 'W05_CLAUDE_CODE_OK'
    root = Path(__file__).resolve().parent.parent
    runtime = Path(tempfile.mkdtemp(prefix='gateway-client-identity-', **({'dir': '/tmp'} if args.check_capture else {})))
    os.chmod(runtime, 0o700)
    captures = runtime / 'captures'
    captures.mkdir()
    fixture = runpy.run_path(str(root / 'scripts/client-roundtrip-mock.py'))['FixtureHandler']
    if args.check_tenant_access:
        fixture = runpy.run_path(str(root / 'scripts/client_tenant_access.py'))['handler'](fixture)
    if args.check_tools:
        fixture = runpy.run_path(str(root / 'scripts/client_tool_roundtrip_fixture.py'))['handler'](fixture)
    if args.check_slow_reader:
        slow_reader = runpy.run_path(str(root / 'scripts/client_slow_reader.py'))
        fixture = slow_reader['handler'](fixture)
    elif args.check_cancel or args.check_long_stream:
        fixture = runpy.run_path(str(root / 'scripts/client_cancel_fixture.py'))['handler'](fixture)
        if args.check_long_stream:
            fixture.stream_seconds = 660
    if args.check_error:
        fixture = runpy.run_path(str(root / 'scripts/client_error_fixture.py'))['handler'](fixture)
        fixture.success_control = args.control_error_as_success
        fixture.auth_recovery = args.check_auth_recovery
        fixture.transient_error = args.check_transient_recovery
        fixture.stream_failure = args.check_stream_failure
        if interrupted_mode:
            fixture.error_marker = 'IDLE_TIMEOUT_STREAM_STARTED' if args.check_transient_recovery else 'INTERRUPTED_STREAM_STARTED'
    if args.check_compact:
        fixture = runpy.run_path(str(root / 'scripts/client_compact_fixture.py'))['handler'](fixture)
        fixture.auto_claude = args.claude_auto_compact
        fixture.compact_file = str(runtime / 'compact-context.txt')
        if args.claude_auto_compact:
            Path(fixture.compact_file).write_text('GATEWAY_CONTEXT ' * 10000)
        fixture.omit_summary = args.control_omit_summary
        fixture.failure = None if args.control_compact_success else args.compact_failure
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
    if args.check_capture:
        environment.update({'CONTENT_CAPTURE_ENABLED': 'true', 'DATABASE_MAX_CONNECTIONS': '8', 'CONTENT_CAPTURE_TEST_ALLOW_LOCAL_WITNESS': '1'})
        for label in ('SPOOL', 'KEY', 'ANCHOR', 'ERASURE', 'WITNESS'):
            directory = runtime / ('encrypted-' + label.lower())
            directory.mkdir(mode=0o700)
            environment['CONTENT_CAPTURE_' + label + '_DIR'] = str(directory)
        initialization = subprocess.run([str(binary), '--initialize-content-capture'], cwd=root, env=environment, capture_output=True, text=True, timeout=30)
        (runtime / 'capture-initialize.log').write_text(initialization.stdout + initialization.stderr)
        if initialization.returncode:
            raise RuntimeError(f'保存初始化失敗，紀錄位於{runtime}')
    if args.check_access_matrix or args.check_tenant_access:
        environment['OPENAI_API_KEY' if codex else 'ANTHROPIC_API_KEY'] = 'fixture-tenant-api'
        fixture.provider_credentials.add('fixture-tenant-api')
    if args.check_tenant_access:
        environment['PROVIDER_ALLOWED_TENANT_IDS'] = '00000000-0000-0000-0000-000000000001,00000000-0000-0000-0000-000000000002'
    if args.check_codex_held:
        environment.update({'CODEX_AUTH_FILE': str(args.codex_auth_file.resolve()), 'CODEX_ACCOUNT_ID': source_auth['tokens']['account_id'], 'CODEX_OWNER_USER_ID': str(uuid.uuid4()), 'PROVIDER_ALLOWED_TENANT_IDS': environment['ADMIN_BOOTSTRAP_TENANT_ID']})
        fixture.provider_credentials.update((source_auth['tokens']['access_token'], source_auth['tokens']['account_id']))
    if args.check_local:
        environment['EXTRA_PROVIDER_PROFILES_JSON'] = json.dumps([{'kind': 'ollama', 'credential_ref': 'env://LOCAL_OLLAMA', 'provider_key': provider_key, 'base_url': 'http://127.0.0.1:11434', 'operations': [operation]}])
    if args.check_slow_reader:
        environment.update({'SCHEDULER_PASSTHROUGH': 'false', 'SCHEDULER_PER_PROVIDER': '1', 'SCHEDULER_PER_MODEL': '1'})
    if args.check_transient_recovery == 'timeout':
        environment['PROVIDER_IDLE_TIMEOUT_MS'] = '5000' if args.control_timeout_not_enforced else '250'
    checks = []
    if owned_cancel:
        cancel_marker = 'owned-cancel-' + uuid.uuid4().hex

    def api(path, method='GET', body=None, token=None, expected=200, raw_response=False, response_metadata=None):
        headers = {'x-request-id': str(uuid.uuid4())}
        if token:
            headers['authorization'] = f'Bearer {token}'
        data = None if body is None else json.dumps(body).encode()
        if data is not None:
            headers['content-type'] = 'application/json'
        try:
            with urlopen(Request(base + path, data=data, headers=headers, method=method), timeout=8) as response:
                status, raw = response.status, response.read()
                request_id = response.headers.get('x-request-id')
        except HTTPError as error:
            status, raw = error.code, error.read()
            request_id = error.headers.get('x-request-id')
        if response_metadata is not None:
            response_metadata.update(status=status, request_id=request_id)
        if expected is not None and status != expected:
            raise RuntimeError(f'{method} {path}: {status}, expected {expected}')
        if raw_response:
            return raw.decode()
        return json.loads(raw) if raw else None

    log = (runtime / 'gateway.log').open('wb')
    gateway = subprocess.Popen(['timeout', '--kill-after=5s', '1500s' if owned_cancel or (args.check_local and args.check_owned_tools) else '900s' if args.check_local or args.check_long_stream else '240s', str(binary)], cwd=root, env=environment, stdout=log, stderr=log)
    try:
        time.sleep(2)
        if gateway.poll() is not None:
            raise RuntimeError(f'Gateway啟動失敗，紀錄位於{runtime}')
        if api('/health')['revision'] != args.revision:
            raise RuntimeError('Gateway版本不符')
        login = api('/admin/v1/bootstrap', 'POST', {'display_name': '隔離管理者', 'credential_name': '隔離登入'}, bootstrap, 201)['login_token']
        admin = api('/admin/v1/sessions', 'POST', token=login, expected=201)['access_token']

        def create(kind, body, token=admin):
            return api('/admin/v1/' + kind, 'POST', body, token, 201)

        def activate(kind, resource, token=admin):
            api(f'/admin/v1/{kind}/{resource["id"]}/lifecycle', 'POST', {'version': resource['version'], 'state': 'active'}, token)

        provider = create('providers', {'provider_key': provider_key, 'display_name': '本機模型' if args.check_local else '本人來源' if args.check_codex_held else '假供應商', 'provider_type': 'local' if args.check_local else 'openai' if codex else 'anthropic', 'base_url': 'http://127.0.0.1:11434' if args.check_local else 'https://chatgpt.com/backend-api/codex' if args.check_codex_held else environment['OPENAI_BASE_URL' if codex else 'ANTHROPIC_BASE_URL'], 'credential_ref': 'env://LOCAL_OLLAMA' if args.check_local else 'env://CODEX_AUTH_FILE' if args.check_codex_held else ('env://OPENAI_API_KEY' if codex else 'env://ANTHROPIC_API_KEY'), 'supported_operations': [operation]})['resource']
        activate('providers', provider)
        users = {}
        credential_versions = {}
        for name in ('alice', 'bob', 'missing'):
            model = model_prefix + '-' + name
            resource = create('models', {'provider_id': provider['id'], 'public_model_id': model, 'provider_model_id': args.source_model if live_source else model, 'supported_operations': [operation], 'supports_streaming': True, 'supports_tools': True, 'supports_reasoning': not args.check_local or 'thinking' in local_capabilities, **({'max_output_tokens': 512, 'context_window': 32768} if args.check_local else {})})['resource']
            activate('models', resource)
            user = create('users', {'display_name': name})['resource']
            created_credential = create('credentials', {'display_name': name, 'user_id': user['id']})
            credential = created_credential['secret']
            credential_versions[name] = (created_credential['resource']['id'], created_credential['resource']['version'])
            fixture.client_credentials.add(credential)
            users[name] = (user['id'], credential)
            policy = create('policies', {'policy_key': name, 'revision': 1, 'user_id': user['id'], 'configuration': {'constraints': {'allowed_models': [model]}}})['resource']
            activate('policies', policy)
            if name != 'missing' and not live_source:
                api(f'/admin/v1/provider-api-keys/{provider_key}/{user["id"]}', 'PUT', {'token': fixture.provider_keys_by_model[model]}, admin)

        if args.check_tenant_access:
            print(f'租戶角色矩陣證據：{runtime}', flush=True)
            gateway.terminate()
            gateway.wait(timeout=7)
            environment['ADMIN_BOOTSTRAP_TENANT_ID'] = '00000000-0000-0000-0000-000000000002'
            gateway = subprocess.Popen(['timeout', '--kill-after=5s', '240s', str(binary)], cwd=root, env=environment, stdout=log, stderr=log)
            time.sleep(2)
            if gateway.poll() is not None or api('/health')['revision'] != args.revision:
                raise RuntimeError('第二租戶Gateway啟動失敗')
            other_login = api('/admin/v1/bootstrap', 'POST', {'display_name': '租戶B管理者', 'credential_name': '租戶B登入'}, bootstrap, 201)['login_token']
            other_admin = api('/admin/v1/sessions', 'POST', token=other_login, expected=201)['access_token']
            matrix = runpy.run_path(str(root / 'scripts/client_tenant_access.py'))['verify']
            rows = matrix(api, create, activate, users, provider, fixture, runtime, admin, other_admin, codex, args.control_tenant_admin_as_viewer)
            print(f'租戶角色矩陣通過{len(rows)}項；證據：{runtime}')
            return

        if args.check_access_matrix:
            print(f'隔離HTTP權限矩陣證據：{runtime}', flush=True)
            matrix_users = {name: (*value, credential_versions[name][0]) for name, value in users.items()}
            matrix = runpy.run_path(str(root / 'scripts/client_access_matrix.py'))['verify']
            rows = matrix(base, api, create, activate, matrix_users, provider, fixture, runtime, admin, codex, args.control_access_foreign_source)
            print(f'隔離HTTP權限矩陣通過{len(rows)}項；證據：{runtime}')
            return

        if args.check_codex_held:
            gateway.terminate()
            gateway.wait(timeout=7)
            environment['CODEX_OWNER_USER_ID'] = users['alice'][0]
            gateway = subprocess.Popen(['timeout', '--kill-after=5s', '240s', str(binary)], cwd=root, env=environment, stdout=log, stderr=log)
            time.sleep(2)
            if gateway.poll() is not None or api('/health')['revision'] != args.revision:
                raise RuntimeError('持有人綁定後Gateway重啟失敗')

        def client(name, model, success, label, tool_prompt=None, cancel=False, error=False):
            before = len(fixture.request_paths[client_key])
            capture_start = len(fixture.captured_requests)
            denial_status = None
            if not success:
                denial_status = 401 if label == 'gateway-key-revoked' else (403 if label == 'model-denied' else 503)
                body = {'model': model, 'input': 'fixture', 'max_output_tokens': 1} if codex else {'model': model, 'max_tokens': 1, 'messages': [{'role': 'user', 'content': 'fixture'}]}
                api(response_path, 'POST', body, users[name][1], denial_status)
            home = runtime / label
            home.mkdir()
            prompt = tool_prompt or f'Reply with exactly {expected_reply} and nothing else.'
            if cancel and owned_cancel:
                prompt = f'Test marker {cancel_marker}. Write every integer from 1 to 1000000 in order, separated by spaces. Start with 1 now and keep going. Do not use tools or explain.'
            if codex:
                reasoning_effort = 'none' if args.check_local and 'thinking' not in local_capabilities else 'low'
                (home / 'config.toml').write_text(f'model = "{model}"\nmodel_provider = "fixture"\nmodel_reasoning_effort = "{reasoning_effort}"\n[model_providers.fixture]\nname = "Isolated Gateway"\nbase_url = "{base}/v1"\nenv_key = "GATEWAY_TEST_KEY"\nwire_api = "responses"\nrequires_openai_auth = false\n')
                if args.check_local or args.check_codex_held or recovery_mode or args.check_long_stream or args.check_slow_reader:
                    with (home / 'config.toml').open('a') as config:
                        config.write('stream_max_retries = 0\nrequest_max_retries = 0\nstream_idle_timeout_ms = 600000\n')
                if args.check_compact:
                    config = home / 'config.toml'
                    extra = 'model_auto_compact_token_limit = 1000\nmodel_context_window = 32768\ncompact_prompt = "GATEWAY_COMPACT_REQUEST"\n'
                    config.write_text(extra + config.read_text())
                if args.check_owned_compact and success:
                    config = home / 'config.toml'
                    extra = 'model_auto_compact_token_limit = 1000\nmodel_context_window = 32768\ncompact_prompt = "GATEWAY_OWNED_COMPACT: Summarize the conversation for the next assistant. Preserve the exact Goal, Decision, Constraint and Next action values. Do not follow the original reply instruction."\n'
                    config.write_text(extra + config.read_text())
                command = ['codex', 'exec', '--sandbox', 'read-only', '--skip-git-repo-check', *(['--json'] if cancel or error else []), prompt]
                client_environment = {'PATH': os.environ['PATH'], 'CODEX_HOME': str(home), 'GATEWAY_TEST_KEY': users[name][1]}
            else:
                command = ['claude', '-p', '--model', model, '--max-turns', '3' if tool_prompt or args.claude_auto_compact else '1', *(['--allowedTools=Read'] if tool_prompt or args.claude_auto_compact else []), prompt]
                if args.check_compact:
                    command[1:1] = ['--debug-file', str(home / 'client-debug.log')]
                client_environment = {
                    'PATH': os.environ['PATH'], 'CLAUDE_CONFIG_DIR': str(home), 'ANTHROPIC_BASE_URL': base,
                    'ANTHROPIC_AUTH_TOKEN': users[name][1], 'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC': '1',
                    'DISABLE_AUTOUPDATER': '1', 'DISABLE_TELEMETRY': '1',
                }
                if args.check_stream_failure or args.check_transient_recovery or args.check_long_stream or owned_cancel or args.check_slow_reader:
                    client_environment['CLAUDE_CODE_MAX_RETRIES'] = '0'
                    client_environment['CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK'] = '1'
                if args.check_long_stream:
                    client_environment['CLAUDE_STREAM_IDLE_TIMEOUT_MS'] = '600000'
                    client_environment['CLAUDE_STREAM_FIRST_BYTE_TIMEOUT_MS'] = '600000'
                if args.check_compact:
                    client_environment['CLAUDE_AUTOCOMPACT_PCT_OVERRIDE'] = '1'
                    client_environment['CLAUDE_CODE_AUTO_COMPACT_WINDOW'] = '100000'
                if owned_cancel and cancel:
                    command[1:1] = ['--output-format', 'stream-json', '--verbose', '--include-partial-messages']
            if cancel and owned_cancel:
                result, fixture.local_cancel_proof = runpy.run_path(str(root / 'scripts/client_local_cancel.py'))['run'](command, client_environment, runtime, home, args.source_model, codex, args.control_force_client_kill)
                fixture.client_exit = result.returncode
            elif cancel:
                process = subprocess.Popen(command, env=client_environment, cwd=runtime, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
                try:
                    if not fixture.stream_started.wait(10):
                        raise RuntimeError('原Client未開始上游串流')
                    if fixture.peer_closed.wait(0.25):
                        raise RuntimeError('取消前上游已中斷')
                    if process.poll() is not None:
                        raise RuntimeError('取消前原Client已退出')
                    os.killpg(process.pid, signal.SIGKILL if args.control_force_client_kill else signal.SIGINT)
                    stdout, stderr = process.communicate(timeout=5)
                    fixture.client_exit = process.returncode
                    diagnostic = stdout + '\n' + stderr
                    for secret in fixture.client_credentials | fixture.provider_credentials:
                        diagnostic = diagnostic.replace(secret, '[credential]')
                    (home / 'client-diagnostic.txt').write_text(diagnostic)
                    codex_interrupted = False
                    if codex and process.returncode == 1:
                        events = [json.loads(line) for line in stdout.splitlines() if line.strip()]
                        kinds = {event.get('type') for event in events}
                        codex_interrupted = {'thread.started', 'turn.started'}.issubset(kinds) and not kinds.intersection({'error', 'turn.failed', 'turn.completed'})
                    if process.returncode not in (0, 130, -signal.SIGINT) and not codex_interrupted:
                        raise RuntimeError(f'原Client未以正常取消方式結束：{process.returncode}')
                    if not fixture.peer_closed.wait(5):
                        raise RuntimeError('取消原Client後上游連線未關閉')
                    result = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
                finally:
                    if process.poll() is None:
                        os.killpg(process.pid, signal.SIGTERM)
                        try:
                            process.wait(timeout=3)
                        except subprocess.TimeoutExpired:
                            os.killpg(process.pid, signal.SIGKILL)
                            process.wait()
            else:
                result = subprocess.run(['timeout', '--kill-after=5s', '780s' if args.check_long_stream else '600s' if args.check_local and success else '30s', *command], env=client_environment, cwd=runtime, capture_output=True, text=True, timeout=790 if args.check_long_stream else 610 if args.check_local and success else 40)
                if args.check_owned_tools and success:
                    initial_diagnostic = result.stdout + '\n' + result.stderr
                    for secret in fixture.client_credentials | fixture.provider_credentials:
                        initial_diagnostic = initial_diagnostic.replace(secret, '[credential]')
                    (home / 'initial-client-diagnostic.txt').write_text(initial_diagnostic)
                    (home / 'initial-client-result.json').write_text(json.dumps({'exit': result.returncode, 'expected_reply_matches': result.stdout.strip() == owned_value}, indent=2))
                    if result.returncode != 0 or result.stdout.strip() != owned_value:
                        raise RuntimeError(f'真工具驗收第一輪未完成：{runtime}')
                    resume = ['codex', 'exec', 'resume', '--last', '--skip-git-repo-check', followup_prompt] if codex else ['claude', '-p', '--continue', '--model', model, '--max-turns', '1', followup_prompt]
                    result = subprocess.run(['timeout', '--kill-after=5s', '600s' if args.check_local else '30s', *resume], env=client_environment, cwd=runtime, capture_output=True, text=True, timeout=610 if args.check_local else 40)
                if args.check_owned_compact and success:
                    initial_diagnostic = result.stdout + '\n' + result.stderr
                    for secret in fixture.client_credentials | fixture.provider_credentials:
                        initial_diagnostic = initial_diagnostic.replace(secret, '[credential]')
                    (home / 'initial-client-diagnostic.txt').write_text(initial_diagnostic)
                    if result.returncode != 0 or result.stdout.strip() != 'ACK':
                        raise RuntimeError(f'真摘要驗收第一輪未完成：{runtime}')
                    resume_prompt = 'Return the four remembered field values in order Goal, Decision, Constraint, Next action, joined by |. Nothing else.'
                    resume = ['codex', 'exec', 'resume', '--last', '--skip-git-repo-check', resume_prompt]
                    result = subprocess.run(['timeout', '--kill-after=5s', '30s', *resume], env=client_environment, cwd=runtime, capture_output=True, text=True, timeout=40)
                if args.check_compact:
                    initial_diagnostic = result.stdout + '\n' + result.stderr
                    for secret in fixture.client_credentials | fixture.provider_credentials:
                        initial_diagnostic = initial_diagnostic.replace(secret, '[credential]')
                    (home / 'initial-client-diagnostic.txt').write_text(initial_diagnostic)
                    initial_reply = expected_reply
                    if not (args.claude_auto_compact and args.compact_failure) and (result.returncode != 0 or result.stdout.strip() != initial_reply):
                        raise RuntimeError('壓縮驗收第一輪原Client未完成')
                    if not codex and not args.claude_auto_compact:
                        compact_command = ['claude', '-p', '--continue', '--model', model, '--max-turns', '1', '/compact GATEWAY_COMPACT_REQUEST']
                        compact_result = subprocess.run(['timeout', '--kill-after=5s', '30s', *compact_command], env=client_environment, cwd=runtime, capture_output=True, text=True, timeout=40)
                        compact_diagnostic = compact_result.stdout + '\n' + compact_result.stderr
                        for secret in fixture.client_credentials | fixture.provider_credentials:
                            compact_diagnostic = compact_diagnostic.replace(secret, '[credential]')
                        (home / 'compact-client-diagnostic.txt').write_text(compact_diagnostic)
                        if compact_result.returncode != 0:
                            raise RuntimeError(f'原Claude手動壓縮未完成：{runtime}')
                    if not args.claude_auto_compact:
                        resume = ['codex', 'exec', 'resume', '--last', '--skip-git-repo-check', prompt] if codex else ['claude', '-p', '--continue', '--model', model, '--max-turns', '1', prompt]
                        result = subprocess.run(['timeout', '--kill-after=5s', '30s', *resume], env=client_environment, cwd=runtime, capture_output=True, text=True, timeout=40)
            after = len(fixture.request_paths[client_key])
            diagnostic = result.stdout + '\n' + result.stderr
            for secret in fixture.client_credentials | fixture.provider_credentials:
                diagnostic = diagnostic.replace(secret, '[credential]')
            (home / 'client-diagnostic.txt').write_text(diagnostic)
            if not cancel:
                (home / 'client-result.json').write_text(json.dumps({'exit': result.returncode, 'expected_success': success and not error, 'expected_reply_matches': result.stdout.strip() == expected_reply}, indent=2))
            if live_source:
                if not cancel and success and (result.returncode != 0 or result.stdout.strip() != expected_reply):
                    raise RuntimeError(f'{label}: 原{args.client}未完成來源實送，紀錄位於{runtime}')
                if not success and (result.returncode != 1 or result.stdout.strip() == expected_reply):
                    raise RuntimeError(f'{label}: 原{args.client}未正常退出拒絕，退出碼{result.returncode}')
            elif args.check_compact and args.compact_failure:
                if result.returncode != 1 or fixture.failure_marker not in diagnostic or result.stdout.strip() == expected_reply:
                    raise RuntimeError(f'原Client未正確回報壓縮失敗：{runtime}')
                observed = fixture.captured_requests[capture_start:]
                if not observed or any(not capture['provider_credential_matches_fixture'] or not capture['client_credential_absent'] for capture in observed):
                    raise RuntimeError('壓縮失敗路徑憑證隔離不符')
            elif success or error:
                if error and (result.returncode != 1 or (not interrupted_mode and not (codex and args.check_transient_recovery == 'rate-limit') and fixture.error_marker not in diagnostic) or not 1 <= after - before <= (1 if recovery_mode else 6)):
                    raise RuntimeError(f'{label}: 原Client未正確回報上游錯誤，退出{result.returncode}、上游{after - before}次')
                if not error and not cancel and (result.returncode != 0 or result.stdout.strip() != expected_reply or after <= before):
                    raise RuntimeError(f'{label}: 原Client未完成假串流')
                observed = fixture.captured_requests[capture_start:]
                if not observed or any(capture['model'] != model or not capture['provider_credential_matches_fixture'] or not capture['client_credential_absent'] for capture in observed):
                    raise RuntimeError(f'{label}: 憑證隔離失敗')
            elif after != before or result.stdout.strip() == expected_reply:
                raise RuntimeError(f'{label}: 應拒絕卻送出或回成功')
            checks.append({'case': label, 'passed': True, 'upstream_requests': None if live_source else after - before, 'client_exit': result.returncode, 'denial_status': denial_status})

        if args.check_compact:
            client('alice', f'{model_prefix}-alice', True, 'compact-continuation')
            result = {'compact_requests': fixture.compact_requests, 'resumed_requests': fixture.resumed_requests}
            (runtime / 'compact-result.json').write_text(json.dumps(result, indent=2))
            if args.compact_failure:
                attempts_valid = fixture.compact_requests == 8 if args.claude_auto_compact else 1 <= fixture.compact_requests <= 6
                if not attempts_valid or fixture.resumed_requests != 0 or checks[0]['upstream_requests'] != (2 if args.claude_auto_compact else 1) + fixture.compact_requests:
                    raise RuntimeError(f'壓縮失敗後錯誤接續或次數無界：{runtime}')
            elif fixture.compact_requests != 1 or fixture.resumed_requests != 1 or checks[0]['upstream_requests'] != (4 if args.claude_auto_compact else 3):
                raise RuntimeError(f'未觀察到原Client壓縮及摘要接續：{runtime}')
            (runtime / 'result.json').write_text(json.dumps({'revision': args.revision, 'client_version': version, 'compact_failure': args.compact_failure, 'compact_trigger': 'reactive' if args.claude_auto_compact else ('automatic' if codex else 'manual'), 'checks': checks, **result}, indent=2))
            outcome = '壓縮失敗處理' if args.compact_failure else '壓縮接續'
            print(f'原{args.client} {outcome}驗收通過；證據：{runtime}')
            return

        def verify_capture():
            validation = runpy.run_path(str(root / 'scripts/client_capture_validation.py'))
            markers = [cancel_marker, expected_reply] if owned_cancel else owned_fields if args.check_owned_compact else [owned_value, expected_reply] if args.check_owned_tools else [expected_reply] if live_source else [fixture.expected_value, fixture.context_marker, expected_reply] if args.check_tools else ([fixture.error_marker, expected_reply] if args.check_error else ['CANCEL_STREAM_PENDING', expected_reply])
            if args.check_slow_reader:
                markers.append('SLOW_READER_RESOURCE_CHECK')
            reject_plaintext = runpy.run_path(str(root / 'scripts/client_capture_validation.py'))['reject_plaintext']
            spool_files = list((runtime / 'encrypted-spool').glob('*.capture'))
            if not spool_files:
                raise RuntimeError('沒有可檢查的加密暫存檔')
            for path in spool_files:
                reject_plaintext(path.read_bytes(), markers)
            query = root / 'queries/test_client_capture_records.sql'
            count = 3 if args.check_owned_compact or args.check_owned_tools else fixture.error_requests if args.check_error else (2 if args.check_tools else 1)
            if recovery_mode:
                count += fixture.recovery_requests
            if cancel_recovery:
                count = 2
            expected_state = 'incomplete' if args.check_cancel else 'complete'
            for _ in range(250):
                result = subprocess.run(['psql', database, '-XAtq', '-v', 'ON_ERROR_STOP=1', '-v', 'user=' + users['alice'][0]], input=query.read_text(), capture_output=True, text=True, timeout=5, check=True)
                records = json.loads(result.stdout)
                states = ['incomplete', 'complete'] if interrupted_mode or cancel_recovery else [expected_state] * count
                if len(records) == count and [record['state'] for record in records] == states:
                    break
                time.sleep(0.2)
            else:
                raise RuntimeError('加密保存數量或完成狀態不符：' + json.dumps(records))
            if {path.stem for path in spool_files} != {record['capture_id'] for record in records}:
                raise RuntimeError('未逐筆檢查所有保存暫存檔')
            saved = []
            for record in records:
                content = {}
                for leg in ('client_request', 'provider_request', 'provider_response', 'client_response'):
                    content[leg] = api(f'/admin/v1/request-captures/{record["capture_id"]}/content?leg={leg}', token=admin, raw_response=True)
                    if not content[leg]:
                        raise RuntimeError('加密讀回內容為空：' + leg)
                if args.check_slow_reader and record['state'] == 'incomplete':
                    slow_reader['validate_prefix'](content['provider_response'], content['client_response'])
                else:
                    validation['validate_passthrough_stream'](content['provider_response'], content['client_response'])
                saved.append(content)
            if args.check_tools:
                runpy.run_path(str(root / 'scripts/client_capture_validation.py'))['validate_tools'](saved, fixture, codex)
            elif args.check_error:
                validation['validate_errors'](saved[:-1] if recovery_mode else saved, fixture.error_payloads)
                for content in saved:
                    for leg in ('client_request', 'provider_request'):
                        if expected_reply not in content[leg]:
                            raise RuntimeError('錯誤保存未保留原請求')
            elif args.check_long_stream:
                validation['validate_long_stream'](saved, fixture.emitted_stream, fixture.elapsed_seconds, expected_reply, codex)
            elif live_source:
                if args.check_owned_compact:
                    compact = runpy.run_path(str(root / 'scripts/client_capture_validation.py'))['validate_owned_compact'](saved, owned_fields)
                    (runtime / 'compact-result.json').write_text(json.dumps(compact, indent=2))
                if args.check_owned_tools:
                    validation['validate_owned_followup'](saved, owned_value, followup_prompt, codex)
                    (runtime / 'owned-tools-result.json').write_text(json.dumps({'source_requests': len(saved), 'original_context_preserved': True, 'tool_result_preserved': True, 'second_user_instruction_preserved': True, 'followup_without_new_tools': True}, indent=2))
                for leg in ('client_request', 'provider_request'):
                    if not (args.check_owned_tools or args.check_owned_compact or owned_cancel) and expected_reply not in saved[0][leg]:
                        raise RuntimeError('本人來源保存缺少原請求')
            else:
                if 'CANCEL_STREAM_PENDING' not in saved[0]['provider_response'] or 'response.completed' in saved[0]['provider_response'] or 'message_stop' in saved[0]['provider_response']:
                    raise RuntimeError('中斷保存內容不符')
            usage_query = root / 'queries/test_client_capture_usage.sql'
            usage_result = subprocess.run(['psql', database, '-XAtq', '-v', 'ON_ERROR_STOP=1', '-v', 'user=' + users['alice'][0]], input=usage_query.read_text(), capture_output=True, text=True, timeout=5, check=True)
            usage_records = json.loads(usage_result.stdout)
            if args.check_long_stream and any(row['status'] != 'completed' for row in usage_records):
                raise RuntimeError('長串流未成功結帳')
            if args.check_error and {row['request_id'] for row in usage_records} != {request_id for check in checks for request_id in check.get('ingress_request_ids', [])}:
                raise RuntimeError('錯誤帳本未涵蓋所有入口請求')
            if live_source:
                (runtime / 'usage-result.json').write_text(json.dumps(usage_records, indent=2))
                if owned_cancel:
                    first = [row for row in usage_records if row['id'] == records[0]['capture_id']]
                    second = [row for row in usage_records if row['id'] == records[1]['capture_id']]
                    validation['validate_owned_cancel'](saved[:1], first, records[:1], args.source_model, cancel_marker, fixture.local_cancel_proof, codex)
                    validation['validate_owned_usage'](saved[1:], second, records[1:], expected_reply, codex)
                    if len(usage_records) != 2 or any(row['model'] != args.source_model or row['provider'] != provider_key for row in usage_records):
                        raise RuntimeError('實際模型取消與恢復不是同一來源各一次')
                else:
                    validation['validate_owned_usage'](saved, usage_records, records, expected_reply, codex)
            elif recovery_mode:
                name = 'openai-response.sse' if codex else 'anthropic-response.sse'
                validation['validate_error_recovery'](saved, fixture.error_payloads, (fixture.fixture_dir / name).read_text(), usage_records, records, codex, interrupted=interrupted_mode, upstream_request_id=fixture.error_request_ids[0], rejection_status=error_status if args.check_transient_recovery and not interrupted_mode else None)
            elif args.check_slow_reader:
                if len(usage_records) != 2 or [row['status'] for row in usage_records] != ['cancelled', 'completed']:
                    raise RuntimeError('慢讀取消與新請求的帳本狀態不符')
                for index, cancel in enumerate((True, False)):
                    selected = [row for row in usage_records if row['id'] == records[index]['capture_id']]
                    validation['validate_usage'](selected, records[index:index + 1], cancel, codex)
            else:
                validation['validate_usage'](usage_records, records, args.check_cancel, codex, args.check_error)
            dump = subprocess.run(['pg_dump', database, '--data-only', '--no-owner', '--no-privileges'], capture_output=True, timeout=15, check=True)
            reject_plaintext(dump.stdout, markers)
            reject_plaintext((runtime / 'gateway.log').read_bytes(), markers)
            (runtime / 'plaintext-result.json').write_text(json.dumps({'spool_files': len(spool_files), 'capture_ids': sorted(path.stem for path in spool_files), 'database_dump_bytes': len(dump.stdout), 'gateway_log_bytes': (runtime / 'gateway.log').stat().st_size, 'markers': len(markers), 'plaintext_absent': True}, indent=2))
            (runtime / 'usage-result.json').write_text(json.dumps(usage_records, indent=2))
            (runtime / 'capture-result.json').write_text(json.dumps({'records': records, 'legs_read': len(saved) * 4, 'stream_legs_equal': all(content['provider_response'] == content['client_response'] for content in saved), 'stream_events_equal': None if args.check_slow_reader else True, 'partial_client_prefix_verified': args.check_slow_reader}, indent=2))
            checks[-1]['capture_records'] = records

        if live_source and not owned_cancel:
            owned_prompt = None
            if args.check_owned_compact:
                owned_fields = [name + '-' + uuid.uuid4().hex for name in ('goal', 'decision', 'constraint', 'next')]
                expected_reply = '|'.join(owned_fields)
                owned_prompt = 'Remember these exact fields for later. ' + ' '.join(name + ': ' + value + '.' for name, value in zip(('Goal', 'Decision', 'Constraint', 'Next action'), owned_fields)) + ' For this turn reply exactly ACK.'
            if args.check_owned_tools:
                owned_value = 'owned-tool-' + uuid.uuid4().hex
                expected_reply = 'FOLLOWUP:' + owned_value
                followup_prompt = 'Using only the tool result from our previous turn, reply with FOLLOWUP: followed immediately by the exact file contents. Do not read the file again or call any tools. Return no other text.'
                owned_file = runtime / 'owned-tool-input.txt'
                owned_file.write_text(owned_value)
                (runtime / 'owned-tools-input.json').write_text(json.dumps({'sha256': hashlib.sha256(owned_file.read_bytes()).hexdigest(), 'bytes': owned_file.stat().st_size, 'source_model': args.source_model, 'client_version': version}, indent=2))
                owned_prompt = f'Use exactly one local shell command to read {owned_file}. Then reply with exactly the file contents and nothing else.'
                if args.check_local and codex:
                    owned_prompt += ' Use the provided exec_command function with cmd set to cat followed by the exact path above. Do not use container.exec, exec or read_file; those tools are not available.'
                if not codex:
                    owned_prompt = f'Use exactly one Read tool call to read {owned_file}. Your final answer must contain only the exact file contents, excluding the Read line number. No acknowledgement, explanations, quotes, Markdown, code fences or follow-up questions.'
            client('alice', f'{model_prefix}-alice', True, 'held-owner', owned_prompt)
            if args.check_capture:
                verify_capture()
            if not args.check_owned_tools:
                if args.check_local:
                    client('alice', f'{model_prefix}-bob', False, 'model-denied')
                else:
                    client('bob', f'{model_prefix}-bob', False, 'held-other-user')
                credential_id, credential_version = credential_versions['alice']
                api(f'/admin/v1/credentials/{credential_id}?version={credential_version}', 'DELETE', token=admin)
                client('alice', f'{model_prefix}-alice', False, 'gateway-key-revoked')
            query = root / 'queries/test_codex_held_result.sql'
            result = subprocess.run(['psql', database, '-XAtq', '-v', 'ON_ERROR_STOP=1', '-v', 'owner=' + users['alice'][0]], input=query.read_text(), capture_output=True, text=True, timeout=5, check=True)
            ledger = json.loads(result.stdout)
            expected_attempts = 3 if args.check_owned_compact or args.check_owned_tools else 1
            if ledger['attempts'] != expected_attempts or ledger['completed_owner'] != expected_attempts or ledger['other_attempts'] != 0 or ledger['reported_records'] != expected_attempts or ledger['reported_input'] <= 0 or ledger['reported_output'] <= 0 or ledger['models'] != [args.source_model] * expected_attempts or ledger['providers'] != [provider_key] * expected_attempts:
                raise RuntimeError('本人來源帳本與隔離不符：' + json.dumps(ledger))
            for index, check in enumerate(checks):
                check['upstream_requests'] = expected_attempts if index == 0 else 0
            if args.check_codex_held and args.codex_auth_file.read_bytes() != source_bytes:
                raise RuntimeError('來源登入檔在驗收期間改變，未宣稱未變更')
            (runtime / 'result.json').write_text(json.dumps({'revision': args.revision, 'client_version': version, 'source_model': args.source_model, 'source_kind': 'ollama' if args.check_local else 'chatgpt', 'source_capabilities': local_capabilities if args.check_local else None, 'source_auth_unchanged': True if args.check_codex_held else None, 'source_sha256': hashlib.sha256(source_bytes).hexdigest() if args.check_codex_held else None, 'checks': checks, 'ledger': ledger, 'not_run': ['model-denied', 'gateway-key-revoked'] if args.check_owned_tools else []}, indent=2))
            outcome = '真工具與續談' if args.check_owned_tools else '真實來源與拒絕'
            print(f'原{args.client}{outcome}驗收通過；證據：{runtime}')
            return

        if args.check_error:
            client('alice', f'{model_prefix}-alice', True, 'upstream-error', error=True)
            ingress = [event for line in (runtime / 'gateway.log').read_text().splitlines() if (event := json.loads(line)).get('http', {}).get('route') == response_path and event.get('http', {}).get('response', {}).get('status_code') == error_status]
            ingress_ids = {event['request_id'] for event in ingress}
            if len(ingress) != fixture.error_requests or len(ingress_ids) != len(ingress):
                raise RuntimeError('入口錯誤請求與上游嘗試數量不符')
            checks[-1]['ingress_request_ids'] = sorted(ingress_ids)
            if codex and args.check_transient_recovery == 'rate-limit':
                diagnostic = (runtime / 'upstream-error/client-diagnostic.txt').read_text()
                runpy.run_path(str(root / 'scripts/client_capture_validation.py'))['validate_rate_limit_diagnostic'](diagnostic, checks[-1]['ingress_request_ids'][0])
            if args.check_transient_recovery == 'timeout':
                closed = fixture.timeout_peer_closed.wait(2)
                idle_proof = {'peer_closed': closed, 'elapsed_seconds': fixture.timeout_elapsed_seconds, 'idle_timeout_ms': int(environment['PROVIDER_IDLE_TIMEOUT_MS']), 'source_wait_seconds': 2}
                (runtime / 'idle-timeout-result.json').write_text(json.dumps(idle_proof, indent=2))
                runpy.run_path(str(root / 'scripts/client_capture_validation.py'))['validate_idle_timeout'](idle_proof)
            if args.check_transient_recovery in ('rate-limit', 'unavailable'):
                time.sleep(1)
            if recovery_mode:
                fixture.recovered = True
                client('alice', f'{model_prefix}-alice', True, 'after-upstream-error')
                recovered_ingress = [event for line in (runtime / 'gateway.log').read_text().splitlines() if (event := json.loads(line)).get('http', {}).get('route') == response_path and event.get('http', {}).get('response', {}).get('status_code') == 200 and event['request_id'] not in ingress_ids]
                if fixture.error_requests != 1 or fixture.recovery_requests != 1 or len(recovered_ingress) != 1 or recovered_ingress[0]['request_id'] in ingress_ids:
                    raise RuntimeError('錯誤後未以唯一新請求恢復')
                checks[-1]['ingress_request_ids'] = [recovered_ingress[0]['request_id']]
            if args.check_capture:
                verify_capture()
            (runtime / 'result.json').write_text(json.dumps({'revision': args.revision, 'client_version': version, 'transient_error': args.check_transient_recovery, 'checks': checks}, indent=2))
            print(f'原Client上游錯誤通過；證據：{runtime}')
            return
        if args.check_long_stream:
            client('alice', f'{model_prefix}-alice', True, 'long-stream')
            if fixture.cancel_requests != 1:
                raise RuntimeError('長串流產生非預期的上游重送')
            verify_capture()
            checks[-1]['elapsed_seconds'] = fixture.elapsed_seconds
            (runtime / 'result.json').write_text(json.dumps({'revision': args.revision, 'client_version': version, 'checks': checks}, indent=2))
            print(f'原Client長串流通過；證據：{runtime}')
            return
        if args.check_cancel:
            if args.check_slow_reader:
                fixture.slow_proof = slow_reader['run'](gateway.pid, binary, base, users['alice'][1], f'{model_prefix}-alice', codex, fixture, runtime, args.control_drain_reader)
                checks.append({'label': 'slow-reader-cancel', 'client_kind': 'raw_http_capacity_probe', 'native_client': False, 'upstream_requests': 1, 'ingress_request_ids': [fixture.slow_proof['ingress_request_id']]})
            else:
                client('alice', f'{model_prefix}-alice', True, 'stream-cancel', cancel=True)
            if not owned_cancel and fixture.cancel_requests != 1:
                raise RuntimeError('取消測試產生非預期的上游重送')
            query = root / 'queries/test_client_cancel_ledger.sql'
            for _ in range(30):
                ledger = subprocess.run(['psql', database, '-XAtq', '-v', 'ON_ERROR_STOP=1', '-v', 'user=' + users['alice'][0]], input=query.read_text(), capture_output=True, text=True, timeout=5, check=True)
                observed = json.loads(ledger.stdout)
                if observed['requests'] == 1 and observed['cancelled'] == 1 and observed['attempts'] == 1:
                    break
                time.sleep(0.1)
            else:
                raise RuntimeError('原Client取消未正確持久記帳：' + json.dumps(observed))
            if cancel_recovery:
                if api('/health').get('active_requests') != 0:
                    raise RuntimeError('原Client取消後Gateway仍有工作')
                client('alice', f'{model_prefix}-alice', True, 'after-local-cancel')
            if args.check_capture:
                verify_capture()
            active = api('/health').get('active_requests')
            if active != 0:
                raise RuntimeError(f'取消後Gateway仍有執行中的請求：{active}')
            checks[0].update({'cancelled_by_sigint': not args.check_slow_reader, 'upstream_closed': None if owned_cancel else True, 'active_requests_after': active, 'ledger': observed})
            if cancel_recovery:
                for check in checks:
                    check['upstream_requests'] = 1
            (runtime / 'result.json').write_text(json.dumps({'revision': args.revision, 'client_version': version, 'source_model': args.source_model, 'local_cancel': fixture.local_cancel_proof if owned_cancel else None, 'slow_reader': fixture.slow_proof if args.check_slow_reader else None, 'checks': checks, 'not_run': ['model-denied', 'gateway-key-revoked'] if cancel_recovery else []}, indent=2))
            print(f'慢讀容量與原Client新請求通過；證據：{runtime}' if args.check_slow_reader else f'原Client串流取消通過；證據：{runtime}')
            return
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
            if args.check_capture:
                verify_capture()
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
    except Exception:
        if args.check_local and args.check_capture and 'admin' in locals() and 'alice' in users:
            try:
                for _ in range(250):
                    result = subprocess.run(['psql', database, '-XAtq', '-v', 'ON_ERROR_STOP=1', '-v', 'user=' + users['alice'][0]], input=(root / 'queries/test_client_capture_records.sql').read_text(), capture_output=True, text=True, timeout=5, check=True)
                    records = json.loads(result.stdout)
                    usage_result = subprocess.run(['psql', database, '-XAtq', '-v', 'ON_ERROR_STOP=1', '-v', 'user=' + users['alice'][0]], input=(root / 'queries/test_client_capture_usage.sql').read_text(), capture_output=True, text=True, timeout=5, check=True)
                    usage_records = json.loads(usage_result.stdout)
                    if {record['capture_id'] for record in records} == {row['id'] for row in usage_records} and (records or not list((runtime / 'encrypted-spool').glob('*.capture'))):
                        break
                    time.sleep(0.2)
                (runtime / 'failure-usage.json').write_text(json.dumps(usage_records, indent=2))
                content = []
                for record in records:
                    legs = {leg: api(f'/admin/v1/request-captures/{record["capture_id"]}/content?leg={leg}', token=admin, raw_response=True) for leg in ('client_request', 'provider_request', 'provider_response', 'client_response')}
                    content.append({'record': record, 'legs': legs})
                (runtime / 'failure-captures.json').write_text(json.dumps(content, indent=2))
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                (runtime / 'failure-capture-error.txt').write_text(type(error).__name__)
        raise
    finally:
        if args.check_long_stream:
            fixture.stop_stream.set()
            (runtime / 'long-stream-result.json').write_text(json.dumps({'elapsed_seconds': fixture.elapsed_seconds, 'upstream_requests': fixture.cancel_requests, 'emitted_stream': fixture.emitted_stream}, indent=2))
            print(f'長串流測試證據：{runtime}')
        if args.check_error:
            (runtime / 'error-result.json').write_text(json.dumps({'upstream_requests': fixture.error_requests, 'recovery_requests': fixture.recovery_requests, 'observations': fixture.error_observations, 'error_payloads': fixture.error_payloads}, indent=2))
            print(f'錯誤測試證據：{runtime}')
        if args.check_cancel:
            fixture.stop_stream.set()
            (runtime / 'cancel-result.json').write_text(json.dumps(getattr(fixture, 'local_cancel_proof', {'client_exit': fixture.client_exit, 'upstream_requests': None if owned_cancel else fixture.cancel_requests, 'upstream_closed': fixture.peer_closed.is_set()}), indent=2))
            print(f'取消測試證據：{runtime}')
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
