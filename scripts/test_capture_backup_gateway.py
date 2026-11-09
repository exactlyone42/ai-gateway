import argparse, hashlib, json, os, select, shutil, signal, subprocess, sys, tempfile, time, uuid
from datetime import datetime, timedelta
from pathlib import Path
parser = argparse.ArgumentParser()
parser.add_argument('--gateway-bin', type=Path, required=True)
parser.add_argument('--credential-environment', choices=('dev', 'prod'), default='dev')
parser.add_argument('--check-protocol-streams', action='store_true')
parser.add_argument('--check-database-privileges', action='store_true')
parser.add_argument('--check-readonly-backup-role', action='store_true')
parser.add_argument('--check-deletion', action='store_true')
parser.add_argument('--check-truncated-spool', action='store_true')
parser.add_argument('--check-wrong-journal', action='store_true')
parser.add_argument('--check-rolled-back-journal', action='store_true')
parser.add_argument('--check-independent-position', action='store_true')
parser.add_argument('--check-remote-witness', action='store_true')
parser.add_argument('--check-remote-replay', action='store_true')
parser.add_argument('--control-remote-rollback', action='store_true')
parser.add_argument('--check-expired-restore', action='store_true')
parser.add_argument('--check-ledger-conflict', action='store_true')
parser.add_argument('--check-backup-entry', action='store_true')
parser.add_argument('--check-instance-lock', action='store_true')
parser.add_argument('--check-client-continuity', action='store_true')
parser.add_argument('--check-lock-timeout', action='store_true')
parser.add_argument('--check-stream-drain', action='store_true')
parser.add_argument('--check-backup-kill', action='store_true')
parser.add_argument('--control-corrupt-ledger-conflict', action='store_true')
parser.add_argument('--check-restored-state', type=Path, help='完整核對還原內容並保留原件；與回退控制合用時應拒絕復活並退出非零')
parser.add_argument('--control-corrupt-restored-state', action='store_true')
arguments = parser.parse_args()
if arguments.control_corrupt_restored_state and not arguments.check_restored_state:
    parser.error('還原內容反例必須搭配 --check-restored-state')
if arguments.check_restored_state and not (arguments.check_remote_replay or arguments.check_expired_restore or arguments.check_ledger_conflict or (arguments.check_remote_witness and arguments.control_remote_rollback)):
    parser.error('完整還原狀態驗收必須完成遠端刪除重放、過期還原或回退反例')
if arguments.check_restored_state and any((arguments.check_client_continuity, arguments.check_stream_drain, arguments.check_backup_kill)):
    parser.error('完整還原狀態驗收須與備份期間持續寫入演練分開')
if arguments.check_restored_state and not any((arguments.check_protocol_streams, arguments.check_expired_restore, arguments.check_ledger_conflict, arguments.control_corrupt_restored_state, arguments.control_remote_rollback)):
    parser.error('完整還原正向驗收必須包含未刪歷史內容，不能用空集合通過')
if arguments.check_readonly_backup_role and not arguments.check_database_privileges:
    parser.error('readonly backup validation requires --check-database-privileges')
if arguments.check_readonly_backup_role and not arguments.check_remote_replay:
    parser.error('readonly backup validation requires --check-remote-replay')
if arguments.check_protocol_streams and not arguments.check_remote_replay:
    parser.error('protocol stream validation requires --check-remote-replay')
if arguments.credential_environment == 'prod' and not (arguments.check_remote_witness or arguments.check_remote_replay):
    parser.error('production validation requires --check-deletion and --check-remote-witness or --check-remote-replay')
if arguments.control_corrupt_ledger_conflict and not arguments.check_ledger_conflict:
    parser.error('corrupt ledger control requires --check-ledger-conflict')
if arguments.check_ledger_conflict and any((arguments.check_deletion, arguments.check_expired_restore, arguments.check_remote_witness, arguments.check_remote_replay, arguments.control_remote_rollback)):
    parser.error('ledger conflict check must run separately')
if arguments.check_expired_restore and any((arguments.check_deletion, arguments.check_truncated_spool, arguments.check_wrong_journal, arguments.check_rolled_back_journal, arguments.check_independent_position, arguments.check_remote_witness, arguments.check_remote_replay, arguments.control_remote_rollback)):
    parser.error('expired restore check must run separately')
if arguments.control_remote_rollback and not arguments.check_remote_witness:
    parser.error('remote rollback control requires --check-remote-witness')
if (arguments.check_truncated_spool or arguments.check_wrong_journal or arguments.check_rolled_back_journal or arguments.check_independent_position or arguments.check_remote_witness or arguments.check_remote_replay) and not arguments.check_deletion:
    parser.error('spool and journal checks require --check-deletion')
if sum((arguments.check_truncated_spool, arguments.check_wrong_journal, arguments.check_rolled_back_journal, arguments.check_independent_position, arguments.check_remote_witness, arguments.check_remote_replay)) > 1:
    parser.error('spool and journal checks must run separately')
binary = arguments.gateway_bin.resolve(strict=True)
if not binary.is_file() or not os.access(binary, os.X_OK):
    raise SystemExit('gateway binary must be an executable file')
project_root = Path(__file__).resolve().parent.parent
os.chdir(project_root)
if arguments.check_restored_state:
    import capture_restore_validation as state_validation
    evidence = arguments.check_restored_state.resolve()
    evidence.mkdir(mode=0o700)
workspace = None if arguments.check_restored_state else tempfile.TemporaryDirectory(prefix='gateway-backup-e2e-')
workspace_path = Path(tempfile.mkdtemp(prefix='gateway-backup-e2e-', dir=evidence) if arguments.check_restored_state else workspace.name)
workspace_path.chmod(0o755)
binary_copy = workspace_path / 'gateway'
shutil.copyfile(binary, binary_copy)
binary_copy.chmod(0o755)
print('gateway sha256: ' + hashlib.sha256(binary_copy.read_bytes()).hexdigest(), flush=True)
expiry = 'expire=' + (datetime.now() + timedelta(days=1)).date().isoformat()
query = (project_root / 'queries/test_capture_backup_latest.sql').read_text()
suffix = uuid.uuid4().hex
run_label = 'gateway.capture_e2e=' + suffix
network = 'capture-e2e-net-' + suffix
project = 'capture-e2e-' + suffix
pg = project + '-postgres-1' if arguments.check_backup_entry else 'capture-e2e-db-' + suffix
gw = 'capture-e2e-gw-' + suffix
mock = 'capture-e2e-mock-' + suffix
volumes = ['capture-e2e-' + suffix + '-' + x for x in ('spool', 'keys', 'anchors', 'ledger', 'erasure', 'witness')]
paths = ['/var/lib/agent-pool-gateway/capture-spool', '/var/lib/agent-pool-gateway/capture-keys', '/var/lib/agent-pool-gateway/capture-anchors', '/var/lib/agent-pool-gateway/ledger-spool', '/var/lib/agent-pool-gateway/capture-erasure', '/var/lib/agent-pool-gateway/capture-witness']
created = []
protocol_captures = []
database_admin = 'fixture_admin' if arguments.check_database_privileges else 'gateway'

def run(*args, **kwargs):
    try:
        return subprocess.run(args, check=True, capture_output=True, text=True, timeout=60, **kwargs).stdout.strip()
    except subprocess.CalledProcessError as error:
        if args[:3] == ('docker', 'network', 'create'):
            raise RuntimeError('隔離網路建立失敗：' + error.stderr.strip()) from None
        raise

def docker(*args, **kwargs):
    return run('docker', *args, **kwargs)

if os.environ.get('DOCKER_HOST'):
    raise SystemExit('DOCKER_HOST is not supported by this isolated test')
context = docker('context', 'show')
os.environ['DOCKER_CONTEXT'] = context
docker_host = docker('context', 'inspect', context, '--format', '{{.Endpoints.docker.Host}}')
if not docker_host.startswith('unix:///'):
    raise SystemExit('this isolated test requires a local Docker socket')
signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))

def api(path, method='GET', token='', body=None, request_id=''):
    if arguments.check_restored_state and path == '/v1/chat/completions' and body is not None:
        body = {'max_tokens': 16, **body}
    code = 'import json,sys,urllib.request; p,m,t,b,q=sys.argv[1:]; h={"authorization":"Bearer "+t,"content-type":"application/json"}; h.update({"x-request-id":q} if q else {}); r=urllib.request.Request("http://127.0.0.1:8080"+p,data=None if b=="" else b.encode(),headers=h,method=m); \ntry: print(urllib.request.urlopen(r,timeout=15).read().decode())\nexcept urllib.error.HTTPError as e: print(e.read().decode(),file=sys.stderr); raise'
    try:
        return json.loads(docker('exec', gw, 'python', '-c', code, path, method, token, '' if body is None else json.dumps(body), request_id))
    except subprocess.CalledProcessError as error:
        raise RuntimeError(f'{path}: {error.stderr}') from None

def api_status(path, method, token):
    code = '''import sys, urllib.error, urllib.request
request = urllib.request.Request("http://127.0.0.1:8080" + sys.argv[1], headers={"authorization": "Bearer " + sys.argv[2]}, method=sys.argv[3])
try:
    print(urllib.request.urlopen(request, timeout=15).status)
except urllib.error.HTTPError as error:
    print(error.code)
'''
    return int(docker('exec', gw, 'python', '-c', code, path, token, method))

def wire_api(path, method, token, body=None):
    if arguments.check_restored_state and path == '/v1/responses' and body is not None:
        body = {'max_output_tokens': 16, **body}
    code = '''import json,sys,urllib.error,urllib.request
p,m,t,b=sys.argv[1:]
request=urllib.request.Request("http://127.0.0.1:8080"+p,data=None if not b else b.encode(),headers={"authorization":"Bearer "+t,"content-type":"application/json"},method=m)
try:
    response=urllib.request.urlopen(request,timeout=15)
except urllib.error.HTTPError as error:
    response=error
with response:
    body=response.read(131073)
    if len(body)>131072:
        raise RuntimeError("fixture response exceeds limit")
    print(json.dumps({"status":response.status,"body":body.decode()}))
'''
    return json.loads(docker('exec', gw, 'python', '-c', code, path, method, token, '' if body is None else json.dumps(body)))

def probe(token, request_id):
    code = '''import json, sys, urllib.error, urllib.request
body = json.dumps({"model": "company-default", "messages": [{"role": "user", "content": "backup-e2e-continuity"}]}).encode()
request = urllib.request.Request("http://127.0.0.1:8080/v1/chat/completions", data=body, method="POST", headers={"authorization": "Bearer " + sys.argv[1], "content-type": "application/json", "x-request-id": sys.argv[2]})
try:
    response = urllib.request.urlopen(request, timeout=20)
    response.read()
    print(response.status, "")
except urllib.error.HTTPError as error:
    print(error.code, error.headers.get("retry-after", ""))
    print(error.read().decode()[:300])
'''
    first, _, detail = docker('exec', gw, 'python', '-c', code, token, request_id).partition('\n')
    status, _, retry_after = first.partition(' ')
    return int(status), retry_after.strip(), detail

def run_with_client_probe(token, command, **kwargs):
    import threading
    results = []
    stop = threading.Event()
    def worker():
        number = 0
        while not stop.is_set():
            number += 1
            request_id = f'continuity-{suffix}-{number}'
            started = time.monotonic()
            status, retry_after, detail = probe(token, request_id)
            results.append((request_id, started, time.monotonic(), status, retry_after, detail))
    thread = threading.Thread(target=worker)
    thread.start()
    try:
        time.sleep(1)
        try:
            run(*command, **kwargs)
        except subprocess.CalledProcessError as error:
            raise RuntimeError(f'backup failed: {error.stderr[-800:]}') from None
        time.sleep(1)
    finally:
        stop.set()
        thread.join(timeout=60)
    return results

def is_deleted(database, capture):
    sql = (project_root / 'queries/test_capture_backup_deleted.sql').read_text()
    status = docker('exec', '-i', database, 'psql', '-U', 'gateway', '-d', 'gateway', '-Atq', '-v', 'ON_ERROR_STOP=1', '-v', 'tenant=' + env['ADMIN_BOOTSTRAP_TENANT_ID'], '-v', 'capture=' + capture, input=sql)
    return status == 't'

def ledger_counts(database, request_id):
    sql = (project_root / 'queries/test_capture_backup_ledger_counts.sql').read_text()
    result = docker('exec', '-i', database, 'psql', '-U', 'gateway', '-d', 'gateway', '-Atq', '-v', 'ON_ERROR_STOP=1', '-v', 'request=' + request_id, input=sql)
    return tuple(map(int, result.split('|')))

def conflict_info(container):
    code = 'import hashlib,json,pathlib,sys; files=list(pathlib.Path(sys.argv[1]).glob("*.conflict")); records=[(p,p.read_bytes()) for p in files]; print(json.dumps([(p.name,hashlib.sha256(b).hexdigest(),json.loads(b.rstrip(bytes([0])))["request"]["request_id"],json.loads(b.rstrip(bytes([0])))["request"]["id"].replace("-", "")) for p,b in records]))'
    return json.loads(docker('exec', container, 'python', '-c', code, paths[3]))
try:
    created.append(('network', network))
    docker('network', 'create', '--internal', '--label', expiry, '--label', run_label, network)
    for v in volumes:
        created.append(('volume', v))
        docker('volume', 'create', '--label', expiry, '--label', run_label, v)
    mounts = []
    for v, p in zip(volumes, paths):
        mounts += ['--mount', f'type=volume,source={v},target={p}']
    docker('run', '--rm', '--label', expiry, '--label', run_label, '--pull=never', '--network', 'none', *mounts, '--entrypoint', 'sh', 'python:3.13-slim', '-c', 'chown -R 10001:999 /var/lib/agent-pool-gateway && chmod 700 /var/lib/agent-pool-gateway/*')
    created.append(('container', pg))
    docker('run', '-d', '--rm', '--label', expiry, '--label', run_label, '--pull=never', '--name', pg, '--network', network, '-e', 'POSTGRES_USER=' + database_admin, '-e', 'POSTGRES_PASSWORD=fixture-password', '-e', 'POSTGRES_DB=gateway', 'postgres:17-alpine')
    for _ in range(100):
        if subprocess.run(['docker', 'exec', pg, 'pg_isready', '-h', '127.0.0.1', '-U', 'gateway'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
            break
        time.sleep(0.2)
    else:
        raise RuntimeError('database not ready')
    if arguments.check_database_privileges:
        fixture = (project_root / 'tests/fixtures/capture-backup/create-database-role.sql').read_text()
        fixture += (project_root / 'tests/fixtures/capture-backup/restrict-database-role.sql').read_text()
        docker('exec', '-i', pg, 'psql', '-X', '-U', database_admin, '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', input=fixture)
    created.append(('container', mock))
    protocol_mounts = ['--mount', f'type=volume,source={volumes[0]},target=/capture-spool,readonly', '--mount', f'type=bind,source={project_root / "tests/fixtures/client-roundtrip"},target=/fixtures,readonly', '-e', 'REQUIRE_CAPTURED_REQUEST=1'] if arguments.check_protocol_streams else []
    docker('run', '-d', '--rm', '--label', expiry, '--label', run_label, '--pull=never', '--name', mock, '--network', network, '--mount', f'type=bind,source={project_root / "tests/fixtures/capture-backup/mock.py"},target=/mock.py,readonly', *protocol_mounts, '--entrypoint', 'python', 'python:3.13-slim', '/mock.py')
    env = {'DATABASE_URL': f'postgres://gateway:fixture-password@{pg}:5432/gateway', 'DATABASE_MAX_CONNECTIONS': '6', 'ADMIN_DATABASE_MAX_CONNECTIONS': '1', 'CREDENTIAL_HASH_KEY': 'fixture-hash-key-with-at-least-32-bytes', 'CREDENTIAL_ENVIRONMENT': 'dev', 'CONTENT_CAPTURE_ENABLED': 'true', 'CONTENT_CAPTURE_SPOOL_DIR': paths[0], 'CONTENT_CAPTURE_KEY_DIR': paths[1], 'CONTENT_CAPTURE_ANCHOR_DIR': paths[2], 'LEDGER_SPOOL_DIR': paths[3], 'ADMIN_BOOTSTRAP_TOKEN': 'fixture-bootstrap-token-at-least-32-bytes', 'ADMIN_BOOTSTRAP_TENANT_ID': '00000000-0000-0000-0000-000000000001', 'LISTEN_ADDRESS': '0.0.0.0:8080', 'OPENAI_BASE_URL': f'http://{mock}:8090', 'OPENAI_API_KEY': 'fixture-provider-key', 'ALLOWED_MODELS': 'company-default'}
    env['CREDENTIAL_ENVIRONMENT'] = arguments.credential_environment
    if arguments.check_protocol_streams:
        env['ANTHROPIC_BASE_URL'] = f'http://{mock}:8090'
        env['ANTHROPIC_API_KEY'] = 'fixture-provider-key'
    env['CONTENT_CAPTURE_ERASURE_DIR'] = paths[4]
    env['CONTENT_CAPTURE_TEST_ALLOW_LOCAL_WITNESS'] = '1'
    env['CONTENT_CAPTURE_WITNESS_DIR'] = paths[5]
    extra_mounts = []
    if arguments.check_remote_witness or arguments.check_remote_replay or arguments.check_expired_restore or arguments.check_ledger_conflict:
        import capture_witness_fixture
        witness_pg, witness_ca, witness_url = capture_witness_fixture.start(docker, run, project_root, workspace_path, network, expiry, run_label, created, suffix)
        env['CONTENT_CAPTURE_REMOTE_WITNESS_URL'] = witness_url
        env.pop('CONTENT_CAPTURE_TEST_ALLOW_LOCAL_WITNESS')
        extra_mounts = ['--mount', f'type=bind,source={witness_ca},target=/tmp/witness-ca.crt,readonly']
    envargs = sum((['-e', k + '=' + v] for k, v in env.items()), [])
    args = ['--pull=never', '--network', network, '--user', '10001:999', *mounts, *extra_mounts, '--mount', f'type=bind,source={binary_copy},target=/tmp/gateway,readonly', *envargs, '--entrypoint', '/tmp/gateway', 'python:3.13-slim']
    docker('run', '--rm', '--label', expiry, '--label', run_label, *args, '--initialize-content-capture')
    if arguments.check_remote_witness or arguments.check_remote_replay or arguments.check_expired_restore or arguments.check_ledger_conflict:
        position = json.loads(docker('run', '--rm', '--label', expiry, '--label', run_label, *args, '--inspect-capture-erasure'))
        capture_witness_fixture.bootstrap(docker, witness_pg, project_root, position)
    created.append(('container', gw))
    docker('run', '-d', '--label', expiry, '--label', run_label, '--name', gw, *args)
    last_health_error = None
    for _ in range(100):
        try:
            api('/health')
            break
        except Exception as error:
            last_health_error = str(error)
            time.sleep(0.2)
    else:
        logs = subprocess.run(['docker', 'logs', gw], capture_output=True, text=True, timeout=30)
        raise RuntimeError('gateway not ready: ' + str(last_health_error) + '; logs: ' + logs.stdout + logs.stderr)
    if arguments.check_database_privileges:
        privileges = (project_root / 'tests/fixtures/capture-backup/database-role-privileges.sql').read_text()
        actual = docker('exec', '-i', pg, 'psql', '-X', '-U', 'gateway', '-d', 'gateway', '-Atq', '-v', 'ON_ERROR_STOP=1', input=privileges)
        if actual != 'f|f|f|f|f|f':
            raise RuntimeError('gateway database role still has elevated privileges: ' + actual)
        identity_query = (project_root / 'queries/capture_backup_database_identity.sql').read_text()
        identity_denied = subprocess.run(['docker', 'exec', '-i', pg, 'psql', '-X', '-U', 'gateway', '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', '-v', 'VERBOSITY=verbose'], input=identity_query, capture_output=True, text=True, timeout=10)
        if identity_denied.returncode != 3 or '42501' not in identity_denied.stderr or 'pg_control_system' not in identity_denied.stderr:
            raise RuntimeError('database identity negative control did not fail on function permission: ' + identity_denied.stderr)
        print('gateway role has no superuser, bypassrls, create-role, create-db, replication or cluster identity access: ok', flush=True)
    if arguments.check_instance_lock:
        second_args = ['CONTENT_CAPTURE_ENABLED=false' if item == 'CONTENT_CAPTURE_ENABLED=true' else item for item in args]
        second = subprocess.run(['docker', 'run', '--rm', '--label', expiry, '--label', run_label, *second_args], capture_output=True, text=True, timeout=60)
        if second.returncode == 0 or 'ledger spool is already in use' not in second.stderr:
            raise RuntimeError('second gateway did not fail on the ledger lock: ' + second.stderr)
        api('/health')
        print('second gateway rejected; first gateway remains healthy: ok', flush=True)
    boot = api('/admin/v1/bootstrap', 'POST', env['ADMIN_BOOTSTRAP_TOKEN'], {'display_name': 'test admin', 'credential_name': 'test admin'})
    session = api('/admin/v1/sessions', 'POST', boot['login_token'])['access_token']
    user = api('/admin/v1/users', 'POST', session, {'display_name': 'test user'})['resource']['id']
    key = api('/admin/v1/credentials', 'POST', session, {'display_name': 'test client', 'user_id': user})['secret']
    if arguments.check_restored_state:
        quota = api('/admin/v1/quotas', 'POST', session, {'quota_key': 'restore-integrity', 'revision': 1, 'user_id': user, 'configuration': {'mode': 'hard', 'limits': {'requests_per_minute': 1000, 'tokens_per_hour': 100000}}})['resource']
        api(f'/admin/v1/quotas/{quota["id"]}/lifecycle', 'POST', session, {'version': quota['version'], 'state': 'active'})
    operations = ['openai_chat_completions', 'openai_responses'] if arguments.check_protocol_streams else ['openai_chat_completions']
    provider = api('/admin/v1/providers', 'POST', session, {'provider_key': 'openai', 'display_name': 'fixture', 'provider_type': 'openai', 'base_url': f'http://{mock}:8090', 'credential_ref': 'env://OPENAI_API_KEY', 'supported_operations': operations})['resource']['id']
    model = api('/admin/v1/models', 'POST', session, {'provider_id': provider, 'public_model_id': 'company-default', 'provider_model_id': 'company-default', 'supported_operations': operations, 'supports_streaming': True})['resource']['id']
    api(f'/admin/v1/providers/{provider}/lifecycle', 'POST', session, {'version': 1, 'state': 'active'})
    api(f'/admin/v1/models/{model}/lifecycle', 'POST', session, {'version': 1, 'state': 'active'})
    result = api('/v1/chat/completions', 'POST', key, {'model': 'company-default', 'messages': [{'role': 'user', 'content': 'backup-e2e-original'}]})
    if result['choices'][0]['message']['content'] != 'fixture reply':
        raise RuntimeError('source response mismatch')
    print('source gateway request: ok', flush=True)
    for _ in range(300):
        capture = docker('exec', '-i', pg, 'psql', '-U', 'gateway', '-d', 'gateway', '-Atq', input=query)
        if capture:
            break
        time.sleep(0.2)
    else:
        raise RuntimeError('capture not imported')
    old = api(f'/admin/v1/request-captures/{capture}/content?leg=client_request', 'GET', session)
    if 'backup-e2e-original' not in json.dumps(old):
        raise RuntimeError('source capture mismatch')
    print('source gateway encrypted capture read: ok', flush=True)
    if arguments.check_protocol_streams:
        anthropic = api('/admin/v1/providers', 'POST', session, {'provider_key': 'anthropic', 'display_name': 'fixture', 'provider_type': 'anthropic', 'base_url': f'http://{mock}:8090', 'credential_ref': 'env://ANTHROPIC_API_KEY', 'supported_operations': ['anthropic_messages']})['resource']['id']
        anthropic_model = api('/admin/v1/models', 'POST', session, {'provider_id': anthropic, 'public_model_id': 'claude-capture', 'provider_model_id': 'claude-test', 'supported_operations': ['anthropic_messages'], 'supports_streaming': True})['resource']['id']
        api(f'/admin/v1/providers/{anthropic}/lifecycle', 'POST', session, {'version': 1, 'state': 'active'})
        api(f'/admin/v1/models/{anthropic_model}/lifecycle', 'POST', session, {'version': 1, 'state': 'active'})
        for endpoint, payload, fixture in (
            ('/v1/responses', {'model': 'company-default', 'input': 'backup-e2e-responses', 'stream': True}, 'openai-response.sse'),
            ('/v1/messages', {'model': 'claude-capture', 'messages': [{'role': 'user', 'content': 'backup-e2e-messages'}], 'max_tokens': 16, 'stream': True}, 'anthropic-response.sse'),
        ):
            previous = docker('exec', '-i', pg, 'psql', '-U', 'gateway', '-d', 'gateway', '-Atq', input=query)
            received = wire_api(endpoint, 'POST', key, payload)
            expected = (project_root / 'tests/fixtures/client-roundtrip' / fixture).read_text()
            if received != {'status': 200, 'body': expected}:
                raise RuntimeError(endpoint + ': fixture stream differs')
            for _ in range(300):
                latest = docker('exec', '-i', pg, 'psql', '-U', 'gateway', '-d', 'gateway', '-Atq', input=query)
                if latest and latest != previous:
                    break
                time.sleep(0.2)
            else:
                raise RuntimeError(endpoint + ': stream capture not imported')
            for leg in ('provider_response', 'client_response'):
                saved = wire_api(f'/admin/v1/request-captures/{latest}/content?leg={leg}', 'GET', session)
                if saved != received:
                    raise RuntimeError(endpoint + ': encrypted stream read differs')
            protocol_captures.append((latest, received))
            counts = json.loads(docker('exec', mock, 'python', '-c', 'import urllib.request; print(urllib.request.urlopen("http://127.0.0.1:8090/count",timeout=5).read().decode())'))
            if counts['captured_requests'] != counts['requests']:
                raise RuntimeError(endpoint + ': provider did not verify saved request')
            docker('exec', '--user', '0', gw, 'chmod', '500', paths[0])
            try:
                denied = wire_api(endpoint, 'POST', key, payload)
                if denied['status'] != 503 or json.loads(denied['body'])['error']['code'] != 'capture_unavailable':
                    raise RuntimeError(endpoint + ': failed storage was not rejected')
            finally:
                docker('exec', '--user', '0', gw, 'chmod', '700', paths[0])
            after = json.loads(docker('exec', mock, 'python', '-c', 'import urllib.request; print(urllib.request.urlopen("http://127.0.0.1:8090/count",timeout=5).read().decode())'))
            if after != counts:
                raise RuntimeError(endpoint + ': failed storage still called provider')
            print(endpoint + ': stream, encrypted read, capture-before-provider and failed-storage denial: ok', flush=True)
    if arguments.check_ledger_conflict:
        install = (project_root / 'tests/fixtures/capture-backup/install-ledger-conflict.sql').read_text()
        remove = (project_root / 'tests/fixtures/capture-backup/remove-ledger-conflict.sql').read_text()
        docker('exec', '-i', pg, 'psql', '-U', 'gateway', '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', input=install)
        result = api('/v1/chat/completions', 'POST', key, {'model': 'company-default', 'messages': [{'role': 'user', 'content': 'backup-e2e-conflict'}]}, 'backup-e2e-conflict')
        if result['choices'][0]['message']['content'] != 'fixture reply':
            raise RuntimeError('conflict fixture request failed')
        for _ in range(100):
            conflicts = conflict_info(gw)
            if conflicts:
                break
            time.sleep(0.1)
        else:
            raise RuntimeError('gateway did not preserve the conflict record')
        if len(conflicts) != 1 or conflicts[0][2] != 'backup-e2e-conflict' or conflicts[0][0] != conflicts[0][3] + '.conflict' or ledger_counts(pg, 'backup-e2e-conflict') != (0, 0, 0):
            raise RuntimeError('conflict fixture was unexpectedly ingested')
        expected_conflict = conflicts[0]
        docker('exec', '-i', pg, 'psql', '-U', 'gateway', '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', input=remove)
        api('/v1/chat/completions', 'POST', key, {'model': 'company-default', 'messages': [{'role': 'user', 'content': 'backup-e2e-before-backup'}]}, 'backup-e2e-before-backup')
        for _ in range(100):
            normal_counts = ledger_counts(pg, 'backup-e2e-before-backup')
            if normal_counts[0] == 1 and normal_counts[1] == 1 and normal_counts[2] >= 1:
                break
            time.sleep(0.1)
        else:
            raise RuntimeError('source gateway did not ingest the normal request after conflict: ' + repr(normal_counts))
        print('source gateway retained conflict and ingested normal usage: ok', flush=True)
    import os, tempfile
    gpg_home = str(workspace_path / 'gnupg')
    os.mkdir(gpg_home)
    os.chmod(gpg_home, 448)
    gpg_env = os.environ.copy()
    gpg_env['GNUPGHOME'] = gpg_home
    gpg_env['GATEWAY_DEPLOYMENT_LOCK'] = str(workspace_path / 'deployment.lock')
    for name, capability in [('fixture-encryption', 'encr'), ('fixture-signing', 'sign')]:
        run('gpg', '--batch', '--pinentry-mode', 'loopback', '--passphrase', '', '--quick-generate-key', f'{name} <{name}@example.invalid>', 'rsa2048', capability, '0', env=gpg_env)

    def fingerprint(name):
        listing = run('gpg', '--batch', '--with-colons', '--list-secret-keys', name, env=gpg_env)
        return next((line.split(':')[9] for line in listing.splitlines() if line.startswith('fpr:')))
    recipient = fingerprint('fixture-encryption@example.invalid')
    signer = fingerprint('fixture-signing@example.invalid')
    backup = str(workspace_path / 'backup')
    if arguments.check_database_privileges:
        denied = Path(backup + '-unauthorized')
        refused = subprocess.run(['python3', 'scripts/capture_backup.py', '--gateway', gw, '--postgres', pg, '--destination', str(denied), '--recipient', recipient, '--signer', signer], env=gpg_env, capture_output=True, text=True, timeout=60)
        if refused.returncode != 1 or denied.exists():
            raise RuntimeError('backup did not reject missing database identity permission before creating output')
        permission = (project_root / 'tests/fixtures/capture-backup/grant-database-identity.sql').read_text()
        docker('exec', '-i', pg, 'psql', '-X', '-U', 'fixture_admin', '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', input=permission)
        print('missing database identity permission rejected without output; granted only pg_control_system execution: ok', flush=True)
    if arguments.check_readonly_backup_role:
        fixture_root = project_root / 'tests/fixtures/capture-backup'
        docker('exec', '-i', pg, 'psql', '-X', '-U', database_admin, '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', input=(fixture_root / 'create-readonly-backup-role.sql').read_text())
        docker('exec', '-i', pg, 'psql', '-X', '-U', 'gateway', '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', input=(fixture_root / 'create-future-backup-schema.sql').read_text())
        observed = docker('exec', '-i', pg, 'psql', '-X', '-U', 'fixture_backup', '-d', 'gateway', '-Atq', '-v', 'ON_ERROR_STOP=1', input=(fixture_root / 'read-future-backup-schema.sql').read_text())
        if observed != '19':
            raise RuntimeError('readonly backup role cannot read future schema contents')
        for name in ('write-backup-probe.sql', 'advance-backup-sequence.sql'):
            rejected = subprocess.run(['docker', 'exec', '-i', pg, 'psql', '-X', '-U', 'fixture_backup', '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', '-v', 'VERBOSITY=verbose'], input=(fixture_root / name).read_text(), capture_output=True, text=True, timeout=10)
            if rejected.returncode != 3 or '42501' not in rejected.stderr:
                raise RuntimeError('readonly backup role did not reject write: ' + name)
        gpg_env['CAPTURE_BACKUP_DATABASE_USER'] = 'fixture_backup'
        docker('exec', '-i', pg, 'psql', '-X', '-U', 'gateway', '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', input=(fixture_root / 'enable-backup-rls.sql').read_text())
        probe = subprocess.run(['docker', 'exec', pg, 'pg_dump', '-U', 'fixture_backup', '-d', 'gateway', '-Fc'], capture_output=True, timeout=30)
        if probe.returncode == 0 or b'row-level security' not in probe.stderr:
            raise RuntimeError('readonly database dump did not reject inaccessible RLS data')
        partial = Path(backup + '-rls-denied')
        refused = subprocess.run(['python3', 'scripts/capture_backup.py', '--gateway', gw, '--postgres', pg, '--destination', str(partial), '--recipient', recipient, '--signer', signer], env=gpg_env, capture_output=True, text=True, timeout=60)
        if refused.returncode != 1 or (partial / '.complete').exists():
            raise RuntimeError('backup did not reject RLS data without publishing completion')
        docker('exec', '-i', pg, 'psql', '-X', '-U', 'gateway', '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', input=(fixture_root / 'disable-backup-rls.sql').read_text())
        print('readonly backup role reads future schema, rejects writes and sequence advance; RLS blocks backup without completion: ok', flush=True)
    if arguments.check_expired_restore:
        expired_capture = str(uuid.uuid4())
        lock_command = ['docker', 'exec', '-i', pg, 'psql', '-X', '-U', 'gateway', '-d', 'gateway', '-Atq', '-v', 'ON_ERROR_STOP=1', '-v', 'tenant=' + env['ADMIN_BOOTSTRAP_TENANT_ID'], '-v', 'user=' + str(uuid.uuid4()), '-v', 'capture=' + expired_capture, '-f', '-']
        maintenance_lock = subprocess.Popen(lock_command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        maintenance_lock.stdin.write((project_root / 'tests/fixtures/capture-backup/seed-expired-locked.sql').read_text())
        maintenance_lock.stdin.flush()
        ready, _, _ = select.select([maintenance_lock.stdout], [], [], 10)
        if not ready or maintenance_lock.stdout.readline().strip() != 'READY' or maintenance_lock.poll() is not None:
            raise RuntimeError('expired capture fixture could not hold maintenance lock')
    if arguments.check_restored_state:
        initial_storage = state_validation.storage_state(docker, gw, paths)
        saved_ids = {name[:-8] for name in initial_storage[paths[0]] if name.endswith('.capture')}
        for _ in range(40):
            before_state = state_validation.database_state(docker, pg)
            complete_ids = {row['capture_id'] for row in before_state['capture_records'] if row['state'] == 'complete'}
            if saved_ids <= complete_ids:
                break
            time.sleep(1)
        else:
            raise RuntimeError('備份前密文暫存尚未完整匯入')
        if not all(before_state[table] for table in ('gateway_requests', 'provider_attempts', 'usage_records', 'quotas', 'quota_windows', 'quota_reservations', 'quota_reconciliation_events', 'capture_records', 'capture_blocks')):
            raise RuntimeError('完整還原驗收缺少非空帳本、額度或密文事件')
        state_validation.write_evidence(evidence, 'source-state', before_state)
        captures_to_read = [row['capture_id'] for row in before_state['capture_records'] if not arguments.check_expired_restore or row['capture_id'] != expired_capture]
        before_content = state_validation.content_state(wire_api, captures_to_read, session)
        state_validation.write_evidence(evidence, 'source-content', before_content)
        before_storage = state_validation.storage_state(docker, gw, paths)
        state_validation.write_evidence(evidence, 'source-storage', before_storage)
        before_position = json.loads(docker('exec', gw, '/proc/1/exe', '--inspect-capture-erasure'))
        state_validation.write_evidence(evidence, 'erasure-position-source', before_position)
        if arguments.check_deletion:
            other_user = api('/admin/v1/users', 'POST', session, {'display_name': 'other owner'})['resource']['id']
            other_key = api('/admin/v1/credentials', 'POST', session, {'display_name': 'other client', 'user_id': other_user})['secret']
            statuses = [api_status(f'/v1/request-captures/{capture}', 'DELETE', credential) for credential in ('', other_key)]
            after_denial = state_validation.database_state(docker, pg)
            state_validation.compare_state(evidence, 'unauthorized-delete-state', before_state, after_denial)
            if statuses != [401, 404]:
                raise RuntimeError('未登入或其他使用者可刪除內容')
            state_validation.write_evidence(evidence, 'unauthorized-delete', statuses)
    if arguments.check_backup_entry:
        backup_root = workspace_path / 'backup-entry'
        backup_env = dict(gpg_env, COMPOSE_PROJECT=project, CAPTURE_BACKUP_GATEWAY=gw, CAPTURE_BACKUP_RECIPIENT=recipient, CAPTURE_BACKUP_SIGNER=signer)
        output = run('bash', 'scripts/backup.sh', 'create', str(backup_root), env=backup_env)
        if '加密備份已建立並驗證' not in output:
            raise RuntimeError('backup entry did not report verified success')
        entries = list(backup_root.iterdir())
        if len(entries) != 1 or not entries[0].is_dir():
            raise RuntimeError('backup entry produced unexpected destinations')
        backup = str(entries[0])
        gateway_id = docker('inspect', '--format', '{{.Id}}', gw)
        postgres_id = docker('inspect', '--format', '{{.Id}}', pg)
        source = run('python3', 'scripts/capture_backup_source_state.py', '--gateway', gw, '--postgres', pg, env=gpg_env).split()
        if len(source) != 6 or source[:3] != ['t', gateway_id, postgres_id]:
            raise RuntimeError('unexpected backup source identity')
        run('python3', 'scripts/capture_backup_freshness.py', str(backup_root), '--signer', signer, '--recipient', recipient, '--gateway-id', gateway_id, '--postgres-id', postgres_id, '--database-system-id', source[3], '--database-oid', source[4], '--storage-id', source[5], '--max-age-hours', '30', env=gpg_env)
        print('signed backup freshness: ok', flush=True)
    elif arguments.check_lock_timeout:
        holder = subprocess.Popen(['docker', 'exec', gw, 'python', '-c', 'import fcntl,sys,time; f=open(sys.argv[1]+"/.capture-backup.lock"); fcntl.flock(f, fcntl.LOCK_SH); print("H", flush=True); time.sleep(14)', paths[0]], stdout=subprocess.PIPE, text=True)
        try:
            if holder.stdout.readline().strip() != 'H':
                raise RuntimeError('lock holder did not start')
            started = time.monotonic()
            refused = subprocess.run(['python3', 'scripts/capture_backup.py', '--gateway', gw, '--postgres', pg, '--destination', backup + '-held', '--recipient', recipient, '--signer', signer], env=gpg_env, capture_output=True, text=True, timeout=60)
            waited = time.monotonic() - started
            if refused.returncode == 0 or waited < 9:
                raise RuntimeError(f'held lock did not make backup fail after waiting: code {refused.returncode}, {waited:.1f}s')
            print(f'backup waited {waited:.1f}s then failed while lock held: ok', flush=True)
        finally:
            holder.wait(timeout=30)
        time.sleep(1)
        run('python3', 'scripts/capture_backup.py', '--gateway', gw, '--postgres', pg, '--destination', backup, '--recipient', recipient, '--signer', signer, env=gpg_env)
        print('backup succeeds after lock released: ok', flush=True)
    elif arguments.check_stream_drain:
        import threading
        stream_id = f'stream-drain-{suffix}'
        code = '''import json, sys, urllib.error, urllib.request
body = json.dumps({"model": "company-default", "stream": True, "messages": [{"role": "user", "content": "backup-e2e-stream"}]}).encode()
request = urllib.request.Request("http://127.0.0.1:8080/v1/chat/completions", data=body, headers={"authorization": "Bearer " + sys.argv[1], "content-type": "application/json", "x-request-id": sys.argv[2]})
try:
    print(urllib.request.urlopen(request, timeout=30).read().decode())
except urllib.error.HTTPError as error:
    print("HTTP", error.code, error.headers.get("retry-after"), error.read().decode())
'''
        streamed = {}
        def stream_worker():
            streamed['body'] = docker('exec', gw, 'python', '-c', code, key, stream_id)
        thread = threading.Thread(target=stream_worker)
        thread.start()
        time.sleep(1)
        started = time.monotonic()
        run('python3', 'scripts/capture_backup.py', '--gateway', gw, '--postgres', pg, '--destination', backup, '--recipient', recipient, '--signer', signer, env=gpg_env)
        waited = time.monotonic() - started
        thread.join(timeout=30)
        if 'fixture end' not in streamed.get('body', ''):
            raise RuntimeError('stream did not finish: ' + streamed.get('body', '')[:200])
        time.sleep(3)
        if ledger_counts(pg, stream_id)[0] != 1:
            raise RuntimeError('stream ledger count is not 1')
        print(f'stream finished intact and backup succeeded after {waited:.1f}s: ok', flush=True)
    elif arguments.check_backup_kill:
        import signal
        victim = subprocess.Popen(['python3', 'scripts/capture_backup.py', '--gateway', gw, '--postgres', pg, '--destination', backup + '-killed', '--recipient', recipient, '--signer', signer], env=gpg_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        deadline = time.monotonic() + 30
        while probe(key, f'kill-wait-{suffix}-{int(time.monotonic() * 1000)}')[0] != 503:
            if time.monotonic() > deadline or victim.poll() is not None:
                raise RuntimeError('backup never held the lock while running')
            time.sleep(0.1)
        os.killpg(victim.pid, signal.SIGKILL)
        victim.wait()
        killed_at = time.monotonic()
        while probe(key, f'kill-after-{suffix}-{int(time.monotonic() * 1000)}')[0] != 200:
            if time.monotonic() - killed_at > 15:
                raise RuntimeError('requests did not resume after the backup process was killed')
            time.sleep(0.2)
        print(f'requests resumed {time.monotonic() - killed_at:.1f}s after backup process was killed: ok', flush=True)
        run('python3', 'scripts/capture_backup.py', '--gateway', gw, '--postgres', pg, '--destination', backup, '--recipient', recipient, '--signer', signer, env=gpg_env)
        print('backup succeeds after a killed backup: ok', flush=True)
    elif arguments.check_client_continuity:
        probes = run_with_client_probe(key, ['python3', 'scripts/capture_backup.py', '--gateway', gw, '--postgres', pg, '--destination', backup, '--recipient', recipient, '--signer', signer], env=gpg_env)
        bad = [item for item in probes if not (item[3] == 200 or (item[3] == 503 and item[4]))]
        if bad or not any(item[3] == 200 for item in probes):
            raise RuntimeError(f'client continuity failed: {[(i[3], i[4], i[5]) for i in bad[:2]]} of {len(probes)}')
        time.sleep(3)
        for request_id, _, _, status, _, _ in probes:
            expected = 1 if status == 200 else 0
            if ledger_counts(pg, request_id)[0] != expected:
                raise RuntimeError(f'ledger mismatch for {request_id}: status {status}')
        refused = [item for item in probes if item[3] == 503]
        codes = sorted({json.loads(item[5])['error']['code'] for item in refused})
        window = max((item[2] - item[1] for item in refused), default=0.0)
        print(f'client continuity: ok, {len(probes)} requests, {len(refused)} refused with retry hint, longest refused {window:.2f}s, codes {codes}', flush=True)
    else:
        run('python3', 'scripts/capture_backup.py', '--gateway', gw, '--postgres', pg, '--destination', backup, '--recipient', recipient, '--signer', signer, env=gpg_env)
    print('real gateway backup: ok', flush=True)
    if arguments.check_rolled_back_journal or arguments.check_independent_position or arguments.check_remote_witness:
        old_erasure = json.loads(docker('exec', gw, 'python', '-c', 'import base64,json,pathlib,sys; print(json.dumps([base64.b64encode(pathlib.Path(p).read_bytes()).decode() for p in sys.argv[1:]]))', paths[4] + '/events', paths[5] + '/head'))
        if arguments.check_independent_position:
            old_position = docker('exec', gw, '/proc/1/exe', '--inspect-capture-erasure')
    run('python3', 'scripts/verify_capture_backup.py', backup, '--signer', signer, env=gpg_env)
    tampered = workspace_path / 'tampered'
    shutil.copytree(backup, tampered)
    altered = tampered / 'gateway.dump.gpg'
    with altered.open('r+b') as artifact:
        artifact.seek(0, os.SEEK_END)
        if artifact.tell() < 2:
            raise RuntimeError('backup database artifact is empty')
        artifact.seek(-1, os.SEEK_END)
        last = artifact.read(1)
        artifact.seek(-1, os.SEEK_END)
        artifact.write(bytes([last[0] ^ 1]))
    rejected = subprocess.run(['python3', 'scripts/verify_capture_backup.py', str(tampered), '--signer', signer], env=gpg_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=60)
    if rejected.returncode != 1 or '備份驗證失敗' not in rejected.stderr:
        raise RuntimeError('tampered backup was not rejected by the backup verifier')
    print('tampered backup rejected: ok', flush=True)
    if arguments.check_deletion:
        if api_status(f'/v1/request-captures/{capture}', 'DELETE', key) not in (202, 204):
            raise RuntimeError('source capture deletion was not accepted')
        for _ in range(150):
            spool_cleared = docker('exec', gw, 'python', '-c', 'import os,sys; print(int(not os.path.lexists(sys.argv[1])))', f'{paths[0]}/{capture}.capture') == '1'
            if is_deleted(pg, capture) and spool_cleared:
                break
            time.sleep(0.2)
        else:
            raise RuntimeError('source deletion did not remove the capture key and spool')
        if api_status(f'/admin/v1/request-captures/{capture}/content?leg=client_request', 'GET', session) not in (404, 410, 503):
            raise RuntimeError('deleted source capture was not denied')
        print('source capture deletion: ok', flush=True)
        if arguments.check_restored_state:
            deleted_position = json.loads(docker('exec', gw, '/proc/1/exe', '--inspect-capture-erasure'))
            state_validation.write_evidence(evidence, 'erasure-position-deleted', deleted_position)
        if arguments.check_remote_witness or arguments.check_remote_replay:
            latest = capture_witness_fixture.read(docker, witness_pg, project_root)
            if latest['install'] != position['install'] or latest['sequence'] <= position['sequence'] or latest['tip'] == bytes(position['tip']).hex():
                raise RuntimeError('remote witness did not advance after deletion')
            print('remote witness advanced after deletion: ok', flush=True)
        if arguments.check_independent_position:
            trusted_position = docker('exec', gw, '/proc/1/exe', '--inspect-capture-erasure')
            docker('exec', gw, '/proc/1/exe', '--inspect-capture-erasure', trusted_position)
    restore_pg = 'capture-e2e-restored-db-' + suffix
    db_volume = 'capture-e2e-restored-' + suffix + '-db'
    created.append(('volume', db_volume))
    docker('volume', 'create', '--label', expiry, '--label', run_label, db_volume)
    db_mount = f'type=volume,source={db_volume},target=/var/lib/postgresql/data'
    created.append(('container', restore_pg))
    docker('run', '-d', '--rm', '--label', expiry, '--label', run_label, '--pull=never', '--name', restore_pg, '--network', 'none', '--mount', db_mount, '-e', 'POSTGRES_USER=' + database_admin, '-e', 'POSTGRES_PASSWORD=fixture-password', '-e', 'POSTGRES_DB=gateway', 'postgres:17-alpine')
    for _ in range(100):
        if subprocess.run(['docker', 'exec', restore_pg, 'pg_isready', '-h', '127.0.0.1', '-U', 'gateway'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
            break
        time.sleep(0.2)
    else:
        raise RuntimeError('restore database not ready')
    if arguments.check_database_privileges:
        fixture = (project_root / 'tests/fixtures/capture-backup/create-database-role.sql').read_text()
        docker('exec', '-i', restore_pg, 'psql', '-X', '-U', database_admin, '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', input=fixture)
    if arguments.check_readonly_backup_role:
        fixture = (project_root / 'tests/fixtures/capture-backup/create-restored-backup-role.sql').read_text()
        docker('exec', '-i', restore_pg, 'psql', '-X', '-U', database_admin, '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', input=fixture)
    target_volumes = ['capture-e2e-restored-' + suffix + '-' + x for x in ('spool', 'keys', 'anchors', 'ledger')]
    for v in target_volumes:
        created.append(('volume', v))
        docker('volume', 'create', '--label', expiry, '--label', run_label, v)
    if arguments.check_backup_entry:
        before = ledger_counts(pg, 'backup-e2e-before-backup') if arguments.check_ledger_conflict else None
        probe = run('python3', '-c', 'import sys; sys.path.insert(0,"scripts"); from restore_capture_backup import preflight,RestoreError\ntry: preflight(sys.argv[1],"gateway","gateway",sys.argv[2:])\nexcept RestoreError as e: print(e)\nelse: raise SystemExit(1)', pg, *target_volumes, env=gpg_env)
        if '目標資料庫必須啟動且隔離網路' not in probe:
            raise RuntimeError('non-isolated source failed for an unexpected reason')
        rejected = subprocess.run(['bash', 'scripts/backup.sh', 'restore-capture', '--backup', backup, '--signer', signer, '--postgres', pg, '--spool', target_volumes[0], '--keys', target_volumes[1], '--anchors', target_volumes[2], '--ledger', target_volumes[3]], env=gpg_env, capture_output=True, text=True, timeout=60)
        if rejected.returncode == 0 or (before is not None and ledger_counts(pg, 'backup-e2e-before-backup') != before):
            raise RuntimeError('restore entry did not protect the non-isolated source')
        for volume in target_volumes:
            docker('run', '--rm', '--label', expiry, '--label', run_label, '--pull=never', '--network', 'none', '--mount', f'type=volume,source={volume},target=/target', '--entrypoint', 'sh', 'postgres:17-alpine', '-c', 'test -z "$(ls -A /target)"')
        print('non-isolated source rejected and preserved: ok', flush=True)
    try:
        restore_command = ('bash', 'scripts/backup.sh', 'restore-capture') if arguments.check_backup_entry else ('python3', 'scripts/restore_capture_backup.py')
        run(*restore_command, '--backup', backup, '--signer', signer, '--postgres', restore_pg, '--spool', target_volumes[0], '--keys', target_volumes[1], '--anchors', target_volumes[2], '--ledger', target_volumes[3], env=gpg_env)
    except subprocess.CalledProcessError as error:
        raise RuntimeError('isolated restore failed: ' + error.stderr) from None
    print('isolated restore: ok', flush=True)
    if arguments.check_restored_state:
        if arguments.control_corrupt_restored_state:
            corrupt = (project_root / 'queries/test_capture_restore_corrupt_quota.sql').read_text()
            changed = docker('exec', '-i', restore_pg, 'psql', '-X', '-U', 'gateway', '-d', 'gateway', '-Atq', '-v', 'ON_ERROR_STOP=1', '-v', 'tenant=' + env['ADMIN_BOOTSTRAP_TENANT_ID'], '-v', 'quota=' + quota['id'], input=corrupt)
            if not changed.isdigit():
                raise RuntimeError('額度反例沒有改到還原資料')
        restored_state = state_validation.database_state(docker, restore_pg)
        state_validation.compare_state(evidence, 'database-restored', before_state, restored_state)
        print('還原前後11表完整內容相同，包含非空額度及密文事件', flush=True)
    if arguments.control_corrupt_ledger_conflict:
        docker('run', '--rm', '--label', expiry, '--label', run_label, '--pull=never', '--network', 'none', '--user', '10001:999', '--mount', f'type=volume,source={target_volumes[3]},target=/ledger', '--entrypoint', 'python', 'python:3.13-slim', '-c', 'import hashlib,pathlib,sys; p=pathlib.Path(sys.argv[1]); assert p.is_file() and hashlib.sha256(p.read_bytes()).hexdigest()==sys.argv[2]; p.write_bytes(b"invalid-ledger-conflict")', '/ledger/' + expected_conflict[0], expected_conflict[1])
    if arguments.check_expired_restore:
        exists_sql = (project_root / 'queries/test_capture_backup_exists.sql').read_text()
        exists = docker('exec', '-i', restore_pg, 'psql', '-U', 'gateway', '-d', 'gateway', '-Atq', '-v', 'ON_ERROR_STOP=1', '-v', 'tenant=' + env['ADMIN_BOOTSTRAP_TENANT_ID'], '-v', 'capture=' + expired_capture, input=exists_sql)
        if exists != '1' or is_deleted(restore_pg, expired_capture):
            raise RuntimeError('backup did not contain the unprocessed expired capture')
        print('backup contains unprocessed expired capture: ok', flush=True)
    if arguments.check_truncated_spool:
        docker('run', '--rm', '--label', expiry, '--label', run_label, '--pull=never', '--network', 'none', '--user', '10001:999', '--mount', f'type=volume,source={target_volumes[0]},target=/spool', '--entrypoint', 'python', 'python:3.13-slim', '-c', 'import pathlib,sys; pathlib.Path(sys.argv[1]).write_bytes(bytes(157))', f'/spool/{capture}.capture')
    if arguments.check_restored_state:
        error = state_validation.collect_gateway_log(evidence, 'gateway-source-log', gw)
        if error:
            raise RuntimeError(error)
    docker('stop', gw)
    docker('rm', '-v', gw)
    created.remove(('container', gw))
    if arguments.check_expired_restore:
        maintenance_lock.stdin.write('\\q\n')
        maintenance_lock.stdin.flush()
        maintenance_lock.stdin.close()
        if maintenance_lock.wait(timeout=10) != 0:
            raise RuntimeError('maintenance lock fixture did not stop')
    docker('stop', restore_pg)
    created.remove(('container', restore_pg))
    restore_pg = 'capture-e2e-restored-restarted-' + suffix
    created.append(('container', restore_pg))
    docker('run', '-d', '--rm', '--label', expiry, '--label', run_label, '--pull=never', '--name', restore_pg, '--network', network, '--mount', db_mount, '-e', 'POSTGRES_USER=gateway', '-e', 'POSTGRES_PASSWORD=fixture-password', '-e', 'POSTGRES_DB=gateway', 'postgres:17-alpine')
    for _ in range(100):
        if subprocess.run(['docker', 'exec', restore_pg, 'pg_isready', '-h', '127.0.0.1', '-U', 'gateway'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
            break
        time.sleep(0.2)
    else:
        raise RuntimeError('restored database restart failed')
    restored_erasure = volumes[4:]
    if arguments.check_rolled_back_journal or arguments.check_independent_position or arguments.check_remote_witness:
        restored_erasure = ['capture-e2e-rollback-' + suffix + '-' + part for part in ('erasure', 'witness')]
        for volume in restored_erasure:
            created.append(('volume', volume))
            docker('volume', 'create', '--label', expiry, '--label', run_label, volume)
        fixture = 'import base64,os,pathlib,sys; a=pathlib.Path("/journal/events"); b=pathlib.Path("/witness/head"); a.write_bytes(base64.b64decode(sys.argv[1])); b.write_bytes(base64.b64decode(sys.argv[2])); [os.chmod(p,0o600) for p in (a,b)]; [os.chown(p,10001,999) for p in (a,b)]; [os.chmod(p,0o700) for p in ("/journal","/witness")]; [os.chown(p,10001,999) for p in ("/journal","/witness")]'
        docker('run', '--rm', '--label', expiry, '--label', run_label, '--pull=never', '--network', 'none', '--mount', f'type=volume,source={restored_erasure[0]},target=/journal', '--mount', f'type=volume,source={restored_erasure[1]},target=/witness', '--entrypoint', 'python', 'python:3.13-slim', '-c', fixture, *old_erasure)
    if arguments.check_wrong_journal:
        restored_erasure = ['capture-e2e-wrong-' + suffix + '-' + part for part in ('erasure', 'witness')]
        for volume in restored_erasure:
            created.append(('volume', volume))
            docker('volume', 'create', '--label', expiry, '--label', run_label, volume)
        fixture = 'import hashlib,os,pathlib,uuid; i=uuid.uuid4().bytes; h=b"APGEJ001"+i; t=hashlib.sha256(h).digest(); w=b"APGEW001"+i+bytes(8)+t; w+=hashlib.sha256(w).digest(); a=pathlib.Path("/journal/events"); b=pathlib.Path("/witness/head"); a.write_bytes(h); b.write_bytes(w); [os.chmod(p,0o600) for p in (a,b)]; [os.chown(p,10001,999) for p in (a,b)]; [os.chmod(p,0o700) for p in ("/journal","/witness")]; [os.chown(p,10001,999) for p in ("/journal","/witness")]'
        docker('run', '--rm', '--label', expiry, '--label', run_label, '--pull=never', '--network', 'none', '--mount', f'type=volume,source={restored_erasure[0]},target=/journal', '--mount', f'type=volume,source={restored_erasure[1]},target=/witness', '--entrypoint', 'python', 'python:3.13-slim', '-c', fixture)
    restore_mounts = []
    for v, p in zip(target_volumes, paths):
        restore_mounts += ['--mount', f'type=volume,source={v},target={p}']
    for v, p in zip(restored_erasure, paths[4:]):
        restore_mounts += ['--mount', f'type=volume,source={v},target={p}']
    env['DATABASE_URL'] = f'postgres://gateway:fixture-password@{restore_pg}:5432/gateway'
    envargs = sum((['-e', k + '=' + v] for k, v in env.items()), [])
    if arguments.control_remote_rollback:
        capture_witness_fixture.execute(docker, witness_pg, project_root / 'tests/fixtures/erasure-witness/rollback-control.sql', install=position['install'], sequence=position['sequence'], tip=bytes(position['tip']).hex())
    if arguments.check_independent_position:
        inspection_mounts = [item if item == '--mount' else item + ',readonly' for item in restore_mounts]
        command = ['docker', 'run', '--rm', '--label', expiry, '--label', run_label, '--pull=never', '--network', 'none', '--user', '10001:999', *inspection_mounts, '--mount', f'type=bind,source={binary_copy},target=/tmp/gateway,readonly', *envargs, '--entrypoint', '/tmp/gateway', 'python:3.13-slim', '--inspect-capture-erasure']
        old_check = subprocess.run(command + [old_position], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=60)
        if old_check.returncode:
            raise RuntimeError('rolled back journal rejected its own position: ' + old_check.stderr)
        latest_check = subprocess.run(command + [trusted_position], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=60)
        if latest_check.returncode == 0 or 'invalid erasure journal' not in latest_check.stderr:
            raise RuntimeError('rolled back journal did not reject the independent latest position: ' + latest_check.stderr)
        print('rolled back journal accepts old position and rejects independent latest position: ok', flush=True)
        raise SystemExit(0)
    if arguments.check_readonly_backup_role:
        fixture = (project_root / 'tests/fixtures/capture-backup/read-restored-backup-role.sql').read_text()
        observed = docker('exec', '-i', restore_pg, 'psql', '-X', '-U', database_admin, '-d', 'gateway', '-Atq', '-v', 'ON_ERROR_STOP=1', input=fixture)
        if observed != 'fixture_backup|19':
            raise RuntimeError('readonly backup omitted future schema or reader permissions during restore')
        print('readonly backup restored future schema contents and reader permissions: ok', flush=True)
    if arguments.check_database_privileges:
        fixture = (project_root / 'tests/fixtures/capture-backup/restrict-database-role.sql').read_text()
        docker('exec', '-i', restore_pg, 'psql', '-X', '-U', database_admin, '-d', 'gateway', '-v', 'ON_ERROR_STOP=1', input=fixture)
        privileges = (project_root / 'tests/fixtures/capture-backup/database-role-privileges.sql').read_text()
        actual = docker('exec', '-i', restore_pg, 'psql', '-X', '-U', 'gateway', '-d', 'gateway', '-Atq', '-v', 'ON_ERROR_STOP=1', input=privileges)
        if actual != 'f|f|f|f|f|f':
            raise RuntimeError('restored gateway database role still has elevated privileges: ' + actual)
        print('restored gateway role has no elevated database privileges before startup: ok', flush=True)
    gw = 'capture-e2e-restored-gw-' + suffix
    created.append(('container', gw))
    if arguments.check_expired_restore:
        restore_lock_command = ['docker', 'exec', '-i', restore_pg, 'psql', '-X', '-U', 'gateway', '-d', 'gateway', '-Atq', '-v', 'ON_ERROR_STOP=1', '-v', 'tenant=' + env['ADMIN_BOOTSTRAP_TENANT_ID'], '-v', 'capture=' + expired_capture, '-f', '-']
        restore_lock = subprocess.Popen(restore_lock_command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        restore_lock.stdin.write((project_root / 'tests/fixtures/capture-backup/lock-expired.sql').read_text())
        restore_lock.stdin.flush()
        ready, _, _ = select.select([restore_lock.stdout], [], [], 10)
        if not ready or restore_lock.stdout.readline().strip() != 'READY' or restore_lock.poll() is not None:
            raise RuntimeError('restored expired capture fixture could not hold row lock')
    docker('run', '-d', '--label', expiry, '--label', run_label, '--pull=never', '--name', gw, '--network', network, '--user', '10001:999', *restore_mounts, *extra_mounts, '--mount', f'type=bind,source={binary_copy},target=/tmp/gateway,readonly', *envargs, '--entrypoint', '/tmp/gateway', 'python:3.13-slim')
    if arguments.check_expired_restore:
        deadline = time.monotonic() + 15
        while True:
            if docker('inspect', '--format', '{{.State.Running}}', gw) != 'true':
                raise RuntimeError('restored gateway stopped before waiting for cleanup')
            if 'capture_startup_cleanup_waiting' in docker('logs', gw):
                break
            if time.monotonic() >= deadline:
                raise RuntimeError('restored gateway never reached cleanup wait')
            time.sleep(0.1)
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            if docker('inspect', '--format', '{{.State.Running}}', gw) != 'true':
                raise RuntimeError('restored gateway stopped while expired capture was locked')
            try:
                api('/health')
            except RuntimeError:
                time.sleep(0.1)
            else:
                raise RuntimeError('restored gateway served while expired capture was locked')
        restore_lock.stdin.write('ROLLBACK;\n\\q\n')
        restore_lock.stdin.flush()
        restore_lock.stdin.close()
        if restore_lock.wait(timeout=10) != 0:
            raise RuntimeError('restored row lock fixture did not stop')
    for _ in range(100):
        try:
            api('/health')
            if arguments.check_wrong_journal or arguments.check_rolled_back_journal or (arguments.check_remote_witness and not arguments.control_remote_rollback):
                raise RuntimeError('restored gateway accepted an invalid erasure journal')
            break
        except RuntimeError as error:
            if (arguments.check_wrong_journal or arguments.check_rolled_back_journal or (arguments.check_remote_witness and not arguments.control_remote_rollback)) and str(error) == 'restored gateway accepted an invalid erasure journal':
                raise
            if (arguments.check_wrong_journal or arguments.check_rolled_back_journal or (arguments.check_remote_witness and not arguments.control_remote_rollback)) and docker('inspect', '--format', '{{.State.Running}}', gw) == 'false':
                logs = subprocess.run(['docker', 'logs', gw], capture_output=True, text=True, timeout=60, check=True)
                code = int(docker('inspect', '--format', '{{.State.ExitCode}}', gw))
                expected = 'invalid erasure journal' if arguments.check_remote_witness else 'capture erasure position mismatch' if arguments.check_wrong_journal else 'capture erasure rollback'
                if code == 0 or expected not in logs.stdout + logs.stderr:
                    raise RuntimeError('restored gateway stopped for an unrelated reason')
                print('restored gateway rejects invalid erasure journal: ok', flush=True)
                raise SystemExit(0)
            time.sleep(0.2)
        except Exception:
            time.sleep(0.2)
    else:
        raise RuntimeError('restored gateway not ready: ' + docker('logs', gw))
    if arguments.check_expired_restore:
        if not is_deleted(restore_pg, expired_capture):
            raise RuntimeError('restored gateway served before expired capture was erased')
        latest = capture_witness_fixture.read(docker, witness_pg, project_root)
        if latest['install'] != position['install'] or latest['sequence'] <= position['sequence']:
            raise RuntimeError('expired capture cleanup did not advance the remote witness')
        print('restored gateway erased expired capture before serving: ok', flush=True)
    if arguments.check_restored_state:
        after_state = state_validation.database_state(docker, restore_pg)
        state_validation.compare_state(evidence, 'ledger-after-startup', before_state, after_state, state_validation.LEDGER_TABLES)
        erased = [capture] if arguments.check_deletion else [expired_capture] if arguments.check_expired_restore else []
        restored_position = json.loads(docker('exec', gw, '/proc/1/exe', '--inspect-capture-erasure'))
        remote_position = capture_witness_fixture.read(docker, witness_pg, project_root)
        state_validation.write_evidence(evidence, 'erasure-position-after', {'local': restored_position, 'remote': remote_position})
        state_validation.check_erased(evidence, after_state, wire_api, session, erased)
        if restored_position['install'] != before_position['install'] or remote_position['install'] != restored_position['install'] or remote_position['sequence'] != restored_position['sequence'] or remote_position['tip'] != bytes(restored_position['tip']).hex():
            raise RuntimeError('還原後本機刪除位置與可信見證不同')
        if arguments.check_deletion and restored_position != deleted_position:
            raise RuntimeError('還原後刪除位置回退或改變')
        if arguments.check_expired_restore and restored_position['sequence'] <= before_position['sequence']:
            raise RuntimeError('過期清理未推進刪除位置')
        remaining = [item for item in captures_to_read if item not in erased]
        if not remaining:
            raise RuntimeError('缺少未刪歷史內容，不能以空集合通過')
        after_content = state_validation.content_state(wire_api, remaining, session)
        expected_content = {item: before_content[item] for item in remaining}
        state_validation.compare_state(evidence, 'content-after', expected_content, after_content)
        before_captures = {table: [row for row in before_state[table] if row['capture_id'] not in erased] for table in ('capture_records', 'capture_blocks')}
        after_captures = {table: [row for row in after_state[table] if row['capture_id'] not in erased] for table in ('capture_records', 'capture_blocks')}
        state_validation.compare_state(evidence, 'capture-after', before_captures, after_captures)
        after_storage = state_validation.storage_state(docker, gw, paths)
        state_validation.check_storage(evidence, before_storage, after_storage, paths, erased, archived=remaining)
        state_summary = {'binary_sha256': hashlib.sha256(binary_copy.read_bytes()).hexdigest(), 'tables': 11, 'source_counts': {name: len(rows) for name, rows in before_state.items()}, 'erased': erased, 'content_legs_preserved': len(remaining) * 4, 'new_key': True, 'scope': '同機隔離容器，非正式或跨主機見證'}
        print('完整帳本/額度、四方向內容、金鑰換新及刪除狀態驗收通過', flush=True)
    if arguments.control_remote_rollback:
        resurrected = api(f'/admin/v1/request-captures/{capture}/content?leg=client_request', 'GET', session)
        if 'backup-e2e-original' not in json.dumps(resurrected):
            raise RuntimeError('rollback control did not restore the old capture')
        print('rollback control: old capture becomes readable when witness also rolls back', flush=True)
        raise SystemExit(0)
    if arguments.check_deletion:
        if arguments.check_truncated_spool:
            contents = docker('exec', gw, 'python', '-c', 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).read_bytes().hex())', f'{paths[0]}/{capture}.capture')
            if contents != '00' * 157:
                raise RuntimeError('truncated spool was not retained')
            print('truncated spool retained after gateway restart: ok', flush=True)
        if not is_deleted(restore_pg, capture):
            raise RuntimeError('deleted capture resurrected after restore')
        if api_status(f'/admin/v1/request-captures/{capture}/content?leg=client_request', 'GET', session) not in (404, 410):
            raise RuntimeError('deleted capture was not denied after restore')
        print('restored gateway preserves deletion: ok', flush=True)
    else:
        restored_old = api(f'/admin/v1/request-captures/{capture}/content?leg=client_request', 'GET', session)
        if old != restored_old:
            raise RuntimeError('restored capture mismatch')
        print('restored gateway reads old capture: ok', flush=True)
    if arguments.check_ledger_conflict and not arguments.control_corrupt_ledger_conflict and conflict_info(gw) != [expected_conflict]:
        raise RuntimeError('restored gateway conflict record changed before new request')
    for saved_id, expected in protocol_captures:
        for leg in ('provider_response', 'client_response'):
            restored = wire_api(f'/admin/v1/request-captures/{saved_id}/content?leg={leg}', 'GET', session)
            if restored != expected:
                raise RuntimeError('restored encrypted stream differs')
    if protocol_captures:
        print('restored Messages and Responses stream captures match: ok', flush=True)
    previous_latest = docker('exec', '-i', restore_pg, 'psql', '-U', 'gateway', '-d', 'gateway', '-Atq', input=query)
    new = api('/v1/chat/completions', 'POST', key, {'model': 'company-default', 'messages': [{'role': 'user', 'content': 'backup-e2e-after-restore'}]}, 'backup-e2e-restored-ledger' if arguments.check_ledger_conflict else '')
    if new['choices'][0]['message']['content'] != 'fixture reply':
        raise RuntimeError('restored response mismatch')
    for _ in range(300):
        latest = docker('exec', '-i', restore_pg, 'psql', '-U', 'gateway', '-d', 'gateway', '-Atq', input=query)
        if latest and latest != previous_latest:
            break
        time.sleep(0.2)
    else:
        raise RuntimeError('new capture not imported')
    new_record = api(f'/admin/v1/request-captures/{latest}/content?leg=client_request', 'GET', session)
    if 'backup-e2e-after-restore' not in json.dumps(new_record):
        raise RuntimeError('new capture mismatch')
    if arguments.control_corrupt_ledger_conflict:
        for _ in range(100):
            logs = subprocess.run(['docker', 'logs', gw], capture_output=True, text=True, timeout=30, check=True)
            if 'ledger_flush_failed' in logs.stdout + logs.stderr and 'ledger record is invalid' in logs.stdout + logs.stderr:
                break
            time.sleep(0.1)
        else:
            raise RuntimeError('corrupt conflict did not stop ledger ingestion')
        if ledger_counts(restore_pg, 'backup-e2e-restored-ledger') != (0, 0, 0):
            raise RuntimeError('corrupt conflict control unexpectedly ingested normal usage')
        retained = docker('exec', gw, 'python', '-c', 'import json,pathlib,sys; p=pathlib.Path(sys.argv[1]); ready=list(p.parent.glob("*.ready")); ids=[json.loads(r.read_bytes().rstrip(bytes([0])))["request"]["request_id"] for r in ready]; print(int(p.read_bytes()==b"invalid-ledger-conflict"), json.dumps(ids))', paths[3] + '/' + expected_conflict[0])
        if retained != '1 ["backup-e2e-restored-ledger"]':
            raise RuntimeError('corrupt conflict or pending normal record was not retained: ' + retained)
        print('negative control: corrupt conflict stops ledger ingestion: ok', flush=True)
    elif arguments.check_ledger_conflict:
        for _ in range(100):
            restored_counts = ledger_counts(restore_pg, 'backup-e2e-restored-ledger')
            if restored_counts[0] == 1 and restored_counts[1] == 1 and restored_counts[2] >= 1:
                break
            time.sleep(0.1)
        else:
            raise RuntimeError('restored gateway did not ingest the normal request usage: ' + repr(restored_counts))
        if conflict_info(gw) != [expected_conflict] or ledger_counts(restore_pg, 'backup-e2e-conflict') != (0, 0, 0):
            raise RuntimeError('restored conflict record was modified or ingested')
        print('restored gateway retained conflict and ingested normal usage: ok', flush=True)
    if arguments.check_restored_state:
        final_state = state_validation.database_state(docker, restore_pg)
        state_summary['new_request'] = state_validation.check_new_accounting(evidence, after_state, final_state, latest)
        state_validation.write_evidence(evidence, 'result', state_summary)
    print('restored gateway writes and reads new capture: ok', flush=True)
finally:
    cleanup_errors = []
    if arguments.check_restored_state:
        error = state_validation.collect_gateway_log(evidence, 'gateway-final-log', gw)
        if error:
            cleanup_errors.append(error)
    for process_name in ('maintenance_lock', 'restore_lock'):
        process = locals().get(process_name)
        if process is not None and process.poll() is None:
            try:
                process.terminate()
                process.wait(timeout=10)
            except (OSError, subprocess.SubprocessError):
                cleanup_errors.append(process_name)
    try:
        remaining = docker('ps', '-aq', '--filter', 'label=' + run_label).splitlines()
        for container in remaining:
            if subprocess.run(['docker', 'rm', '-f', '-v', container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30).returncode:
                cleanup_errors.append(container)
    except (OSError, subprocess.SubprocessError):
        cleanup_errors.append('container cleanup')
    for kind in ('container', 'volume', 'network'):
        for current, name in reversed(created):
            if current != kind:
                continue
            try:
                inspect = ['docker', 'inspect', name] if kind == 'container' else ['docker', kind, 'inspect', name]
                details = subprocess.run(inspect, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=15)
                if details.returncode != 0:
                    continue
                item = json.loads(details.stdout)[0]
                labels = item.get('Config', {}).get('Labels', {}) if kind == 'container' else item.get('Labels', {})
                if (labels or {}).get('gateway.capture_e2e') != suffix:
                    cleanup_errors.append(name)
                    continue
                removal = ['docker', 'rm', '-f', '-v', name] if kind == 'container' else ['docker', kind, 'rm', name]
                result = subprocess.run(removal, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
                if result.returncode:
                    inspect = ['docker', 'inspect', name] if kind == 'container' else ['docker', kind, 'inspect', name]
                    exists = subprocess.run(inspect, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15).returncode == 0
                    if exists:
                        cleanup_errors.append(name)
            except (OSError, subprocess.SubprocessError, ValueError, KeyError, IndexError):
                cleanup_errors.append(name)
    if 'gpg_env' in locals():
        try:
            subprocess.run(['gpgconf', '--kill', 'gpg-agent'], env=gpg_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
        except (OSError, subprocess.SubprocessError):
            cleanup_errors.append('test GPG agent')
    if arguments.check_restored_state:
        print('演練原件已保留：' + str(evidence), flush=True)
    else:
        workspace.cleanup()
    if cleanup_errors:
        message = 'isolated resources could not be removed: ' + ', '.join(cleanup_errors)
        if sys.exc_info()[0] is not None and not (isinstance(sys.exc_info()[1], SystemExit) and sys.exc_info()[1].code == 0):
            print(message, file=sys.stderr)
        else:
            raise RuntimeError(message)
