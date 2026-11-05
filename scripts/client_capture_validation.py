import json


def validate_owned_usage(saved, usage_records, records, expected_reply):
    if not saved or len(saved) != len(records) or len(usage_records) != len(records) or len({row['id'] for row in usage_records}) != len(records) or {row['id'] for row in usage_records} != {record['capture_id'] for record in records}:
        raise RuntimeError('本人來源保存與用量數量不符')
    rows = {row['id']: row for row in usage_records}
    for index, (content, record) in enumerate(zip(saved, records)):
        row = rows[record['capture_id']]
        events = [json.loads(line[5:].lstrip()) for line in content['provider_response'].splitlines() if line.startswith('data:') and line[5:].strip() not in ('', '[DONE]')]
        completed = [event['response'] for event in events if event.get('type') == 'response.completed']
        texts = [event.get('text') for event in events if event.get('type') == 'response.output_text.done']
        if len(completed) != 1 or (index == len(saved) - 1 and texts != [expected_reply]):
            raise RuntimeError('本人來源缺少完整完成回應')
        usage = completed[0]['usage']
        reported = [item for item in row['usage'] or [] if item['source'] == 'provider_reported']
        if row['status'] != 'completed' or len(reported) != 1:
            raise RuntimeError('本人來源完成或實際用量記錄不符')
        details = usage.get('input_tokens_details') or {}
        expected_details = {'cached_input_tokens': details.get('cached_tokens', 0), 'cache_write_input_tokens': details.get('cache_write_tokens', 0), 'reasoning_tokens': (usage.get('output_tokens_details') or {}).get('reasoning_tokens', 0)}
        for actual in (row['actual_usage'], reported[0]):
            if any(actual.get(key) != value for key, value in expected_details.items()):
                raise RuntimeError('本人來源快取或推理用量與供應商回應不一致')
            if usage['input_tokens'] <= 0 or actual['input_tokens'] + actual['cached_input_tokens'] + actual['cache_write_input_tokens'] != usage['input_tokens'] or usage['output_tokens'] <= 0 or actual['output_tokens'] + actual['reasoning_tokens'] != usage['output_tokens']:
                raise RuntimeError('本人來源帳本用量與保存的供應商回應不一致')
        estimates = [item for item in row['usage'] or [] if item['source'] == 'estimated']
        if len(estimates) != 1 or row['estimated_usage'].get('input_tokens', 0) <= 0 or any(estimates[0][key] != row['estimated_usage'].get(key) for key in ('input_tokens', 'output_tokens')):
            raise RuntimeError('本人來源預估用量未獨立記帳')


def validate_owned_tools(saved, expected_value):
    if len(saved) != 2:
        raise RuntimeError('本人來源工具往返不是兩次請求')
    events = [json.loads(line[5:].lstrip()) for line in saved[0]['provider_response'].splitlines() if line.startswith('data:') and line[5:].strip() not in ('', '[DONE]')]
    calls = [event['item'] for event in events if event.get('type') == 'response.output_item.done' and event.get('item', {}).get('type') == 'function_call']
    if len(calls) != 1:
        raise RuntimeError('本人來源未完成唯一工具呼叫')
    for leg in ('client_request', 'provider_request'):
        original = [item for item in json.loads(saved[0][leg]).get('input', []) if item.get('role') == 'user']
        following = json.loads(saved[1][leg]).get('input', [])
        if not original or not all(item in following for item in original):
            raise RuntimeError('本人來源工具保存遺失原訊息')
        outputs = [item for item in following if item.get('type') == 'function_call_output' and item.get('call_id') == calls[0]['call_id']]
        if len(outputs) != 1 or expected_value not in json.dumps(outputs[0].get('output')):
            raise RuntimeError('本人來源工具結果與呼叫未對應')


def validate_tools(saved, fixture, codex):
    if len(fixture.emitted_streams) != 2:
        raise RuntimeError('工具串流來源數量不符')
    for index, content in enumerate(saved):
        if content['provider_response'] != fixture.emitted_streams[index]:
            raise RuntimeError('保存串流與實際上游不一致')
        for leg in ('client_request', 'provider_request'):
            body = json.loads(content[leg])
            messages = body.get('input' if codex else 'messages', [])
            users_saved = [message for message in messages if isinstance(message, dict) and message.get('role') == 'user']
            if not fixture.original_user_messages or not all(original in users_saved for original in fixture.original_user_messages):
                raise RuntimeError('保存未保留完整原訊息：' + leg)
            if index == 1:
                if codex:
                    outputs = [item for item in messages if isinstance(item, dict) and item.get('type') == 'function_call_output' and item.get('call_id') == fixture.tool_call_id]
                else:
                    outputs = [item for message in messages for item in message.get('content', []) if isinstance(item, dict) and item.get('type') == 'tool_result' and item.get('tool_use_id') == fixture.tool_call_id and not item.get('is_error')]
                if not any(fixture.expected_value in json.dumps(output) for output in outputs):
                    raise RuntimeError('保存未保留真實工具結果：' + leg)


def validate_usage(usage_records, records, cancel, codex, error=False):
    if len(usage_records) != len(records) or len({row['id'] for row in usage_records}) != len(records) or {row['id'] for row in usage_records} != {row['capture_id'] for row in records}:
        raise RuntimeError('用量與保存請求不一致')
    expected_actual = None if error or (cancel and codex) else (1, 0 if cancel else 1)
    for row in usage_records:
        if error and row['status'] != 'failed':
            raise RuntimeError('上游錯誤未記為失敗請求')
        reported = [usage for usage in row['usage'] or [] if usage['source'] == 'provider_reported']
        if expected_actual is None:
            if reported or any(row['actual_usage'].values()):
                raise RuntimeError('供應商未提供用量卻被記為實際值')
        elif len(reported) != 1 or (reported[0]['input_tokens'], reported[0]['output_tokens']) != expected_actual or (row['actual_usage'].get('input_tokens', 0), row['actual_usage'].get('output_tokens', 0)) != expected_actual:
            raise RuntimeError('供應商實際用量不符：' + json.dumps(row))
        estimates = [usage for usage in row['usage'] or [] if usage['source'] == 'estimated']
        if not {'input_tokens', 'output_tokens'}.issubset(row['estimated_usage']) or row['estimated_usage']['input_tokens'] <= 0:
            raise RuntimeError('非空請求缺少有效預估用量')
        if len(estimates) != 1 or any(estimates[0][key] != row['estimated_usage'].get(key, 0) for key in ('input_tokens', 'output_tokens')):
            raise RuntimeError('預估用量與帳本不一致')


def reject_plaintext(data, markers):
    if not markers or any(not marker for marker in markers):
        raise RuntimeError('明文檢查缺少對照內容')
    for marker in markers:
        raw = marker.encode()
        if raw in data or raw.hex().encode() in data.lower():
            raise RuntimeError('保存或一般紀錄發現明文')


def validate_errors(saved, payloads):
    if not payloads or len(set(payloads)) != len(payloads):
        raise RuntimeError('錯誤來源缺少獨立回應識別')
    if sorted(content['provider_response'] for content in saved) != sorted(payloads):
        raise RuntimeError('錯誤保存與實際上游回應未逐筆相符')
