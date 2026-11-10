import hashlib
import json
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parent.parent
LEGS = ('client_request', 'provider_request', 'provider_response', 'client_response')
LEDGER_TABLES = (
    'gateway_requests', 'provider_attempts', 'usage_records',
    'request_parameter_snapshots', 'request_attribution', 'quotas',
    'quota_windows', 'quota_reservations', 'quota_reconciliation_events',
)


def write_evidence(directory, name, value):
    with (directory / (name + '.json')).open('x') as target:
        json.dump(value, target, indent=2, ensure_ascii=False)
        target.write('\n')


def collect_gateway_log(directory, name, gateway):
    try:
        logs = subprocess.run(['docker', 'logs', gateway], capture_output=True, text=True, timeout=10)
        write_evidence(directory, name, {'container': gateway, 'exit': logs.returncode,
                                        'stdout': logs.stdout, 'stderr': logs.stderr})
        if logs.returncode:
            return 'Gateway取證命令失敗'
    except (OSError, subprocess.SubprocessError) as error:
        return 'Gateway取證失敗：' + type(error).__name__
    return None


def database_state(docker, postgres):
    query = (ROOT / 'queries/test_capture_backup_state.sql').read_text()
    return json.loads(docker('exec', '-i', postgres, 'psql', '-X', '-U', 'gateway',
                             '-d', 'gateway', '-Atq', '-v', 'ON_ERROR_STOP=1', input=query))


def content_state(wire_api, captures, token):
    result = {}
    for capture in captures:
        result[capture] = {}
        for leg in LEGS:
            response = wire_api(f'/admin/v1/request-captures/{capture}/content?leg={leg}', 'GET', token)
            if response['status'] != 200:
                raise RuntimeError('合法內容未能讀回：' + capture + '/' + leg)
            body = response['body'].encode()
            result[capture][leg] = {'bytes': len(body), 'sha256': hashlib.sha256(body).hexdigest()}
    return result


def compare_state(directory, name, before, after, tables=None):
    tables = before.keys() if tables is None else tables
    changed = [table for table in tables if before[table] != after[table]]
    write_evidence(directory, name, {'before': before, 'after': after, 'changed': changed})
    if changed:
        raise RuntimeError('還原資料內容不一致：' + ', '.join(changed))


def storage_state(docker, gateway, paths):
    code = '''import hashlib,json,pathlib,stat,sys
result={}
for directory in sys.argv[1:]:
    root=pathlib.Path(directory)
    files=sorted(root.iterdir())
    if len(files)>256:
        raise RuntimeError("驗收檔案數超過上限")
    result[directory]={}
    for path in files:
        info=path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_size>64*1024*1024:
            raise RuntimeError("驗收只接受有界一般檔案")
        digest=hashlib.sha256()
        with path.open("rb") as source:
            while block:=source.read(65536):
                digest.update(block)
        result[directory][path.name]={"sha256":digest.hexdigest(),"bytes":info.st_size,"mode":stat.S_IMODE(info.st_mode)}
    if root.name=="capture-keys":
        result[directory]["current_fingerprint"]=(root/"current").read_bytes().hex()
print(json.dumps(result))
'''
    return json.loads(docker('exec', gateway, 'python', '-c', code, *paths))


def check_storage(directory, before, after, paths, erased, archived=()):
    expected_spool = {name: value for name, value in before[paths[0]].items()
                      if name.endswith('.capture') and name[:-8] not in erased}
    actual_spool = {name: value for name, value in after[paths[0]].items() if name.endswith('.capture')}
    old_keys = {name: value for name, value in before[paths[1]].items() if name.endswith('.key')}
    retained_keys = {name: after[paths[1]].get(name) for name in old_keys}
    fresh_key = after[paths[1]]['current_fingerprint']
    write_evidence(directory, 'storage-after', {'before': before, 'after': after, 'erased': erased})
    required = {name for name in expected_spool if name[:-8] not in archived}
    if not required <= actual_spool.keys() or any(expected_spool.get(name) != value for name, value in actual_spool.items()):
        raise RuntimeError('還原後密文暫存不一致或已刪內容復活')
    if retained_keys != old_keys or fresh_key == before[paths[1]]['current_fingerprint']:
        raise RuntimeError('還原未保留舊金鑰或未換新金鑰')
    if fresh_key + '.key' not in after[paths[1]]:
        raise RuntimeError('還原後缺少目前金鑰')
    body = b'APGANC01' + bytes.fromhex(fresh_key) + bytes(9)
    expected_anchor = {'sha256': hashlib.sha256(body + hashlib.sha256(body).digest()).hexdigest(),
                       'bytes': 81, 'mode': 0o600}
    if after[paths[2]].get(fresh_key + '.anchor') != expected_anchor:
        raise RuntimeError('新金鑰的計數紀錄不正確')
    suffixes = ('.ready', '.conflict', '.working', '.reserved', '.next')
    previous_ledger = {name: value for name, value in before[paths[3]].items() if name.endswith(suffixes)}
    restored_ledger = {name: value for name, value in after[paths[3]].items() if name.endswith(suffixes)}
    if previous_ledger != restored_ledger:
        raise RuntimeError('入帳暫存內容在還原後改變')


def check_erased(directory, database, wire_api, token, captures):
    results = []
    for capture in captures:
        records = [row for row in database['capture_records'] if row['capture_id'] == capture]
        blocks = [row for row in database['capture_blocks'] if row['capture_id'] == capture]
        statuses = {leg: wire_api(f'/admin/v1/request-captures/{capture}/content?leg={leg}', 'GET', token)['status'] for leg in LEGS}
        valid = len(records) == 1 and records[0]['state'] == 'deleted' and records[0]['wrapped_key'] is None and not blocks and all(code in (404, 410) for code in statuses.values())
        results.append({'capture': capture, 'records': records, 'blocks': len(blocks), 'statuses': statuses, 'erased': valid})
    write_evidence(directory, 'erasure-after', results)
    if any(not result['erased'] for result in results):
        raise RuntimeError('已刪或過期內容在還原後仍可讀取')


def check_new_accounting(directory, before, after, capture):
    state = {'before': before, 'after': after, 'capture': capture}
    write_evidence(directory, 'new-request-state', state)
    added = {row['id'] for row in after['gateway_requests']} - {row['id'] for row in before['gateway_requests']}
    if added != {capture}:
        raise RuntimeError('還原後新請求未正確入帳')
    reported = [row for row in after['usage_records'] if row['gateway_request_id'] == capture and row['source'] == 'provider_reported']
    reservations = [row for row in after['quota_reservations'] if row['gateway_request_id'] == capture]
    if len(reported) != 1 or (reported[0]['input_tokens'], reported[0]['output_tokens']) != (1, 1):
        raise RuntimeError('還原後新請求的實際用量不符')
    if len(reservations) != 1 or reservations[0]['state'] != 'reconciled' or reservations[0]['actual_tokens'] != 2:
        raise RuntimeError('還原後新請求未正確結帳')
    events = [row for row in after['quota_reconciliation_events'] if row['reservation_id'] == reservations[0]['id']]
    if len(events) != 1 or events[0]['actual_tokens'] != 2:
        raise RuntimeError('還原後新請求重複或漏結帳')
    requests_before = sum(row['reserved_requests'] for row in before['quota_windows'])
    requests_after = sum(row['reserved_requests'] for row in after['quota_windows'])
    if requests_after != requests_before + 1:
        raise RuntimeError('還原後新請求額度不是只增加一次')
    token_windows_before = [row for row in before['quota_windows'] if row['limit_kind'] == 'tokens_per_hour']
    token_windows_after = [row for row in after['quota_windows'] if row['limit_kind'] == 'tokens_per_hour']
    if not token_windows_before or not token_windows_after:
        raise RuntimeError('還原驗收缺少每小時文字用量的額度紀錄')
    tokens_before = sum(row['reserved_tokens'] for row in token_windows_before)
    tokens_after = sum(row['reserved_tokens'] for row in token_windows_after)
    if tokens_after != tokens_before + 2:
        raise RuntimeError('還原後文字用量額度不是只增加實際用量')
    return {'capture': capture, 'actual_tokens': 2, 'final_events': 1,
            'requests_before': requests_before, 'requests_after': requests_after,
            'tokens_before': tokens_before, 'tokens_after': tokens_after}
