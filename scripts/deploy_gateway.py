#!/usr/bin/env python3
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import time
import urllib.request
import uuid


def run(args, **kwargs):
    result = subprocess.run(args, capture_output=True, timeout=180, **kwargs)
    if result.returncode:
        raise RuntimeError(f'{args[0]} 執行失敗，結束碼 {result.returncode}')
    return result.stdout


def inspect(name):
    return json.loads(run(['docker', 'inspect', name]))[0]


def fetch(base, route):
    with urllib.request.urlopen(base + route, timeout=5) as response:
        return json.load(response)


def smoke(base, revision):
    fetch(base, '/ready')
    health = fetch(base, '/health')
    if health.get('revision') != revision:
        raise RuntimeError('服務版本不符')
    return health


def migration_manifest(text):
    manifest = {}
    for line in text.splitlines():
        checksum, filename = line.split()
        version = Path(filename).name.split('_', 1)[0]
        if version in manifest or not re.fullmatch(r'[0-9a-f]{96}', checksum):
            raise RuntimeError('映像內遷移清單無效')
        manifest[version] = checksum
    return manifest


def verify_migrations(manifest, applied):
    actual = {}
    for line in applied.splitlines():
        version, checksum, success = line.split('|')
        if success != 't':
            raise RuntimeError('正式資料庫有未完成的遷移')
        actual[version] = checksum
    if not actual or actual != manifest:
        raise RuntimeError('遷移內容或版本集合不同，禁止啟動候選版；新增遷移須先另外完成相容性驗收')


def replace_route(config, old_port, new_port):
    pattern = rf'(reverse_proxy\s+127\.0\.0\.1:){old_port}(?=\s|$)'
    if len(re.findall(pattern, config)) != 1:
        raise RuntimeError('無法唯一確認 Gateway 入口，未修改設定')
    return re.sub(pattern, lambda match: match[1] + str(new_port), config, count=1)


def client_probe(base, evidence, label):
    settings = json.dumps({'env': {'ANTHROPIC_BASE_URL': base}})
    result = subprocess.run([
        'claude', '-p', '--tools', '', '--strict-mcp-config', '--mcp-config',
        '{"mcpServers":{}}', '--setting-sources', 'user', '--settings', settings,
        '--no-session-persistence', '--max-turns', '1',
        'Reply exactly GATEWAY_DEPLOY_OK',
    ], capture_output=True, timeout=180)
    (evidence / f'{label}.out').write_bytes(result.stdout)
    (evidence / f'{label}.err').write_bytes(result.stderr)
    if result.returncode or result.stdout.strip() != b'GATEWAY_DEPLOY_OK':
        raise RuntimeError('原 Claude Client 實送驗收失敗')


def start_stream_probe(base, evidence):
    output = evidence / 'cutover-stream.jsonl'
    with output.open('wb') as stdout, (evidence / 'cutover-stream.err').open('wb') as stderr:
        process = subprocess.Popen([
            'claude', '-p', '--tools', '', '--strict-mcp-config', '--mcp-config',
            '{"mcpServers":{}}', '--setting-sources', 'user', '--settings',
            json.dumps({'env': {'ANTHROPIC_BASE_URL': base}}),
            '--no-session-persistence', '--max-turns', '1', '--output-format', 'stream-json',
            '--verbose', '--include-partial-messages',
            'Write exactly 200 numbered lines explaining how to keep a service running during deployment. '
            'Do not use tools. End the entire answer with GATEWAY_STREAM_END.',
        ], stdout=stdout, stderr=stderr)
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError('串流在切換前已結束，無法驗證跨切換；未切換入口')
        if 'content_block_delta' in output.read_text(errors='replace'):
            return process
        time.sleep(0.25)
    process.terminate()
    process.wait(timeout=15)
    raise RuntimeError('未收到原 Client 串流，未切換入口')


def finish_stream_probe(process, evidence):
    if process.wait(timeout=180):
        raise RuntimeError('原 Client 跨切換串流失敗')
    records = [json.loads(line) for line in (evidence / 'cutover-stream.jsonl').read_text().splitlines()]
    results = [item for item in records if item.get('type') == 'result']
    if len(results) != 1 or results[0].get('is_error') or 'GATEWAY_STREAM_END' not in results[0].get('result', ''):
        raise RuntimeError('原 Client 跨切換串流未完整結束')


def retire_idle_source(container, port):
    for _ in range(3):
        if fetch(f'http://127.0.0.1:{port}', '/health').get('active_requests') != 0:
            return False
        connections = run(['docker', 'exec', container, 'cat', '/proc/net/tcp', '/proc/net/tcp6']).decode()
        for line in connections.splitlines():
            fields = line.split()
            if len(fields) > 3 and fields[1].endswith(':1F90') and fields[3] == '01':
                return False
        time.sleep(0.25)
    run(['docker', 'stop', '--time', '30', container])
    return True


def reload_caddy(caddy, config_file, previous, replacement):
    if config_file.read_text() != previous:
        raise RuntimeError('入口設定已被其他工作修改，停止切換')
    run(['docker', 'exec', '-i', caddy, 'caddy', 'validate', '--config', '-',
         '--adapter', 'caddyfile'], input=replacement.encode())
    try:
        with config_file.open('w') as handle:
            handle.write(replacement)
            handle.flush()
            os.fsync(handle.fileno())
        run(['docker', 'exec', caddy, 'caddy', 'reload', '--config', '/etc/caddy/Caddyfile'])
    except BaseException:
        config_file.write_text(previous)
        run(['docker', 'exec', caddy, 'caddy', 'reload', '--config', '/etc/caddy/Caddyfile'])
        raise


def deploy(args):
    evidence = Path(args.evidence)
    evidence.mkdir(parents=True, exist_ok=False, mode=0o700)
    source = inspect(args.source_container)
    image = inspect(args.image)
    old_port = source['NetworkSettings']['Ports']['8080/tcp'][0]['HostPort']
    if old_port == str(args.port):
        raise RuntimeError('候選版不可使用舊版 PORT')
    config_file = Path(args.caddyfile)
    previous = config_file.read_text()
    replacement = replace_route(previous, old_port, args.port)
    (evidence / 'Caddyfile.before').write_text(previous)
    env = dict(item.split('=', 1) for item in source['Config']['Env'])
    from urllib.parse import urlsplit
    database = urlsplit(env['DATABASE_URL'])
    if not re.fullmatch(r'[a-zA-Z0-9_]+', database.path[1:]):
        raise RuntimeError('資料庫名稱無法驗證')
    network = next(iter(source['NetworkSettings']['Networks']))
    database_network = inspect(args.database_container)['NetworkSettings']['Networks'].get(network, {})
    hosts = [args.database_container, database_network.get('IPAddress'), *database_network.get('Aliases', [])]
    if not database_network or database.hostname not in hosts or database.port not in (None, 5432):
        raise RuntimeError('預檢資料庫與服務實際連線不同，拒絕部署')
    applied = run(['docker', 'exec', args.database_container, 'psql', '-X', '-v',
                   'ON_ERROR_STOP=1', '-U', database.username, '-d', database.path[1:],
                   '-Atc', "SELECT version, encode(checksum,'hex'), success FROM _sqlx_migrations ORDER BY version"]).decode()
    name = 'gateway-release-' + args.revision + '-' + uuid.uuid4().hex[:8]
    run(['docker', 'create', '--name', name, '--network', next(iter(source['NetworkSettings']['Networks'])),
         '--cpus', '2', '--memory', '768m', '--restart', 'no', image['Id']])
    run(['docker', 'cp', name + ':/app/migrations.sha384', str(evidence / 'migrations.sha384')])
    manifest = migration_manifest((evidence / 'migrations.sha384').read_text())
    verify_migrations(manifest, applied)
    (evidence / 'applied-migrations.txt').write_text(applied)
    base = f'http://127.0.0.1:{args.port}'
    candidate = name + '-serving'
    env['LEDGER_SPOOL_DIR'] = '/var/lib/agent-pool-gateway/ledger-spool'
    env_file = evidence / 'candidate.env'
    env_file.write_text('\n'.join(k + '=' + v for k, v in env.items()) + '\n')
    env_file.chmod(0o600)
    run(['docker', 'run', '-d', '--name', candidate, '--network', next(iter(source['NetworkSettings']['Networks'])),
         '--cpus', '2', '--memory', '768m', '--restart', 'no', '--env-file', str(env_file),
         '-p', f'127.0.0.1:{args.port}:8080', '-v', candidate + '-spool:/var/lib/agent-pool-gateway/ledger-spool', image['Id']])
    deadline = time.monotonic() + 45
    while True:
        if not inspect(candidate)['State']['Running']:
            raise RuntimeError('候選版啟動失敗；舊版與入口未更動')
        try:
            smoke(base, args.revision)
            break
        except (OSError, ValueError):
            if time.monotonic() >= deadline:
                raise RuntimeError('候選版未就緒；舊版與入口未更動')
            time.sleep(1)
    client_probe(base, evidence, 'candidate-client')
    run(['docker', 'update', '--restart', 'unless-stopped', candidate])
    stream = start_stream_probe(args.public_url, evidence)
    try:
        reload_caddy(args.caddy_container, config_file, previous, replacement)
        try:
            smoke(args.public_url, args.revision)
            client_probe(args.public_url, evidence, 'public-client')
            finish_stream_probe(stream, evidence)
        except BaseException:
            reload_caddy(args.caddy_container, config_file, replacement, previous)
            raise
    finally:
        if stream.poll() is None:
            stream.terminate()
            stream.wait(timeout=15)
    receipt = {'revision': args.revision, 'image': image['Id'], 'container': candidate,
               'port': args.port, 'previous_container': args.source_container,
               'public_url': args.public_url, 'accepted': True, 'old_stopped': False}
    (evidence / 'accepted.json').write_text(json.dumps(receipt, indent=2))
    try:
        receipt['old_stopped'] = retire_idle_source(args.source_container, old_port)
    except (RuntimeError, OSError, ValueError, subprocess.TimeoutExpired):
        print('無法確認舊版已無請求，保留舊版供檢查。')
    (evidence / 'accepted.json').write_text(json.dumps(receipt, indent=2))
    print('新版已通過原 Client、公開入口與跨切換串流驗收；舊版停止：' + str(receipt['old_stopped']))


def main():
    parser = argparse.ArgumentParser(description='新版驗收後才切換入口；失敗保留舊版')
    parser.add_argument('--image', required=True)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--source-container', required=True)
    parser.add_argument('--port', required=True, type=int)
    parser.add_argument('--evidence', required=True)
    parser.add_argument('--caddyfile', default='/home/cash/services/caddy/Caddyfile')
    parser.add_argument('--caddy-container', default='caddy')
    parser.add_argument('--database-container', default='agent-pool-gateway-postgres-1')
    parser.add_argument('--public-url', default='https://gateway.exactlyone.dev')
    args = parser.parse_args()
    if not re.fullmatch(r'[a-f0-9]{12,40}', args.revision) or not 1024 <= args.port <= 65535:
        parser.error('版本須為 Git commit，PORT 須在 1024 到 65535')
    lock_file = Path.home() / '.local/state/gateway-deployment.lock'
    lock_file.parent.mkdir(parents=True, exist_ok=True)
    with lock_file.open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        deploy(args)


if __name__ == '__main__':
    try:
        main()
    except (RuntimeError, OSError, ValueError, subprocess.TimeoutExpired) as error:
        raise SystemExit(str(error))
