import json


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


def validate_usage(usage_records, records, cancel, codex):
    if len(usage_records) != len(records) or len({row['id'] for row in usage_records}) != len(records) or {row['id'] for row in usage_records} != {row['capture_id'] for row in records}:
        raise RuntimeError('用量與保存請求不一致')
    expected_actual = None if cancel and codex else (1, 0 if cancel else 1)
    for row in usage_records:
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
