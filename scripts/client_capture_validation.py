import json


def validate_passthrough_stream(provider, client):
    provider_frames = [frame for frame in provider.split('\n\n') if frame != ': keepalive']
    client_frames = [frame for frame in client.split('\n\n') if frame != ': keepalive']
    if provider_frames != client_frames:
        raise RuntimeError('供應商與Client保存串流不一致')


def validate_long_stream(saved, emitted, elapsed, expected_reply, codex):
    if len(saved) != 1 or elapsed is None or elapsed <= 600:
        raise RuntimeError('長串流未以單一請求跨過600秒門檻')
    validate_passthrough_stream(emitted, saved[0]['provider_response'])
    events = [json.loads(line[5:].lstrip()) for line in emitted.splitlines() if line.startswith('data:')]
    completed = 'response.completed' if codex else 'message_stop'
    text = ''.join(event['delta'] if codex else event['delta']['text'] for event in events if event.get('type') == ('response.output_text.delta' if codex else 'content_block_delta'))
    if text != expected_reply or sum(event.get('type') == completed for event in events) != 1 or any(event.get('type') in ('error', 'response.failed', 'response.incomplete') for event in events):
        raise RuntimeError('長串流缺少唯一完成事件或完整原回覆')


def validate_owned_cancel(saved, usage_records, records, model, marker, proof, codex):
    if len(saved) != 1 or len(records) != 1 or len(usage_records) != 1 or records[0].get('state') != 'incomplete':
        raise RuntimeError('本人來源取消缺少唯一不完整保存')
    row = usage_records[0]
    if row['id'] != records[0]['capture_id'] or row['status'] != 'cancelled' or row.get('model') != model or row.get('provider') != 'local-ollama':
        raise RuntimeError('本人來源取消的模型或帳本狀態不符')
    if not proof.get('during', {}).get('is_processing') or proof['during'].get('n_decoded', 0) < 8 or proof.get('after', {}).get('is_processing') is not False or not 0 <= proof.get('stop_seconds', -1) <= 10:
        raise RuntimeError('缺少實際推理開始及取消後停止的證據')
    for leg in ('client_request', 'provider_request'):
        if not marker or marker not in saved[0][leg]:
            raise RuntimeError('本人來源取消保存缺少原指令')
    if json.loads(saved[0]['provider_request']).get('model') != model:
        raise RuntimeError('本人來源取消未送到指定模型')
    validate_passthrough_stream(saved[0]['provider_response'], saved[0]['client_response'])
    events = [json.loads(line[5:].lstrip()) for line in saved[0]['provider_response'].splitlines() if line.startswith('data:')]
    text = [event.get('delta') if codex else (event.get('delta') or {}).get('text') for event in events if event.get('type') == ('response.output_text.delta' if codex else 'content_block_delta')]
    if not any(isinstance(value, str) and value for value in text) or any(event.get('type') in ('message_stop', 'response.completed', 'response.failed', 'response.incomplete', 'error') for event in events):
        raise RuntimeError('本人來源取消沒有部分文字或已經結束')
    native = {}
    for event in events:
        fragment = (event.get('response') or {}).get('usage') if codex else (event.get('message') or {}).get('usage') if event.get('type') == 'message_start' else event.get('usage')
        if fragment:
            native.update(fragment)
    expected = {'input_tokens': native.get('input_tokens', 0), 'output_tokens': native.get('output_tokens', 0), 'cached_input_tokens': (native.get('input_tokens_details') or {}).get('cached_tokens', 0) if codex else native.get('cache_read_input_tokens', 0), 'cache_write_input_tokens': (native.get('input_tokens_details') or {}).get('cache_write_tokens', 0) if codex else native.get('cache_creation_input_tokens', 0), 'reasoning_tokens': (native.get('output_tokens_details') or {}).get('reasoning_tokens', 0) if codex else 0}
    if codex:
        expected['input_tokens'] -= expected['cached_input_tokens'] + expected['cache_write_input_tokens']
        expected['output_tokens'] -= expected['reasoning_tokens']
    reported = [item for item in row['usage'] or [] if item['source'] == 'provider_reported']
    if any(expected.values()):
        if len(reported) != 1 or any(actual.get(key, 0) != value for actual in (row['actual_usage'], reported[0]) for key, value in expected.items()):
            raise RuntimeError('本人來源取消遺失或改寫已回報用量')
    elif reported or any(row['actual_usage'].values()):
        raise RuntimeError('本人來源取消尚無回報用量卻虛構實際值')
    estimates = [item for item in row['usage'] or [] if item['source'] == 'estimated']
    if len(estimates) != 1 or row['estimated_usage'].get('input_tokens', 0) <= 0 or any(estimates[0][key] != row['estimated_usage'].get(key, 0) for key in ('input_tokens', 'output_tokens')):
        raise RuntimeError('本人來源取消未保留獨立估算')


def validate_owned_compact(saved, fields):
    if len(saved) != 3 or len(fields) != 4 or len(set(fields)) != 4:
        raise RuntimeError('真摘要驗收缺少三輪或四項不同內容')
    events = [json.loads(line[5:].lstrip()) for line in saved[1]['provider_response'].splitlines() if line.startswith('data:') and line[5:].strip() not in ('', '[DONE]')]
    summary = '\n'.join(event['text'] for event in events if event.get('type') == 'response.output_text.done')
    if not summary or any(value not in summary for value in fields):
        raise RuntimeError('真摘要遺失目標、決策、限制或下一步')
    for leg in ('client_request', 'provider_request'):
        inputs = [json.dumps(json.loads(content[leg]).get('input', []), ensure_ascii=False) for content in saved]
        if any(value not in inputs[0] for value in fields) or 'GATEWAY_OWNED_COMPACT' not in inputs[1]:
            raise RuntimeError('真摘要請求未保留原內容或摘要指令')
        if json.dumps(summary, ensure_ascii=False)[1:-1] not in inputs[2]:
            raise RuntimeError('真摘要未完整帶入接續請求')
    return {'compact_requests': 1, 'resumed_requests': 1, 'preserved_fields': len(fields), 'summary_characters': len(summary)}


def validate_owned_usage(saved, usage_records, records, expected_reply, codex=True):
    if not saved or len(saved) != len(records) or len(usage_records) != len(records) or len({row['id'] for row in usage_records}) != len(records) or {row['id'] for row in usage_records} != {record['capture_id'] for record in records}:
        raise RuntimeError('本人來源保存與用量數量不符')
    rows = {row['id']: row for row in usage_records}
    for index, (content, record) in enumerate(zip(saved, records)):
        row = rows[record['capture_id']]
        events = [json.loads(line[5:].lstrip()) for line in content['provider_response'].splitlines() if line.startswith('data:') and line[5:].strip() not in ('', '[DONE]')]
        if codex:
            completed = [event['response'] for event in events if event.get('type') == 'response.completed']
            texts = [event.get('text') for event in events if event.get('type') == 'response.output_text.done']
        else:
            completed = [event for event in events if event.get('type') == 'message_stop']
            starts = [event['message']['usage'] for event in events if event.get('type') == 'message_start']
            ends = [event['usage'] for event in events if event.get('type') == 'message_delta']
            if len(starts) != 1 or len(ends) != 1:
                raise RuntimeError('本人來源缺少唯一開始與結束用量')
            native = starts[0] | ends[0]
            completed = [{'usage': {
                'input_tokens': native['input_tokens'] + native.get('cache_read_input_tokens', 0) + native.get('cache_creation_input_tokens', 0),
                'output_tokens': native['output_tokens'],
                'input_tokens_details': {'cached_tokens': native.get('cache_read_input_tokens', 0), 'cache_write_tokens': native.get('cache_creation_input_tokens', 0)},
            }}] if len(completed) == 1 else []
            texts = [''.join(event['delta']['text'] for event in events if event.get('type') == 'content_block_delta' and event.get('delta', {}).get('type') == 'text_delta')]
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


def validate_owned_tools(saved, expected_value, codex=True):
    if len(saved) != 2:
        raise RuntimeError('本人來源工具往返不是兩次請求')
    events = [json.loads(line[5:].lstrip()) for line in saved[0]['provider_response'].splitlines() if line.startswith('data:') and line[5:].strip() not in ('', '[DONE]')]
    calls = [event['item'] for event in events if event.get('type') == 'response.output_item.done' and event.get('item', {}).get('type') == 'function_call'] if codex else [event['content_block'] for event in events if event.get('type') == 'content_block_start' and event.get('content_block', {}).get('type') == 'tool_use']
    if len(calls) != 1:
        raise RuntimeError('本人來源未完成唯一工具呼叫')
    for leg in ('client_request', 'provider_request'):
        field = 'input' if codex else 'messages'
        original = [item for item in json.loads(saved[0][leg]).get(field, []) if item.get('role') == 'user']
        following = json.loads(saved[1][leg]).get(field, [])
        if not original or not all(item in following for item in original):
            raise RuntimeError('本人來源工具保存遺失原訊息')
        if codex:
            outputs = [item for item in following if item.get('type') == 'function_call_output' and item.get('call_id') == calls[0]['call_id']]
        else:
            outputs = [block for item in following if isinstance(item.get('content'), list) for block in item['content'] if block.get('type') == 'tool_result' and block.get('tool_use_id') == calls[0]['id'] and not block.get('is_error')]
        if len(outputs) != 1 or expected_value not in json.dumps(outputs[0].get('output' if codex else 'content')):
            raise RuntimeError('本人來源工具結果與呼叫未對應')
    return calls[0]


def validate_owned_followup(saved, expected_value, followup_prompt, codex=True):
    if len(saved) != 3:
        raise RuntimeError('本人來源工具與續談不是三次請求')
    source_call = validate_owned_tools(saved[:2], expected_value, codex)
    validate_owned_tools([saved[0], saved[2]], expected_value, codex)
    for leg in ('client_request', 'provider_request'):
        field = 'input' if codex else 'messages'
        previous = json.loads(saved[1][leg]).get(field, [])
        following = json.loads(saved[2][leg]).get(field, [])
        if codex:
            calls = [item for item in previous if item.get('type') == 'function_call']
            resumed_calls = [item for item in following if item.get('type') == 'function_call']
            outputs = [item for item in previous if item.get('type') == 'function_call_output']
            resumed_outputs = [item for item in following if item.get('type') == 'function_call_output']
        else:
            calls = [{key: value for key, value in block.items() if key != 'cache_control'} for item in previous if isinstance(item.get('content'), list) for block in item['content'] if block.get('type') == 'tool_use']
            resumed_calls = [{key: value for key, value in block.items() if key != 'cache_control'} for item in following if isinstance(item.get('content'), list) for block in item['content'] if block.get('type') == 'tool_use']
            outputs = [{key: value for key, value in block.items() if key != 'cache_control'} for item in previous if isinstance(item.get('content'), list) for block in item['content'] if block.get('type') == 'tool_result']
            resumed_outputs = [{key: value for key, value in block.items() if key != 'cache_control'} for item in following if isinstance(item.get('content'), list) for block in item['content'] if block.get('type') == 'tool_result']
        identifier = 'call_id' if codex else 'id'
        if len(calls) != 1 or calls != resumed_calls or calls[0].get(identifier) != source_call.get(identifier) or calls[0].get('name') != source_call.get('name'):
            raise RuntimeError('續談未完整保留原工具要求')
        if outputs != resumed_outputs:
            raise RuntimeError('續談未完整保留原工具結果')
        users = [item.get('content') for item in following if item.get('role') == 'user']
        texts = []
        for content in users:
            if isinstance(content, str):
                texts.append(content)
            elif isinstance(content, list):
                texts.append(''.join(block.get('text', '') for block in content if block.get('type') in ('text', 'input_text')))
        if followup_prompt not in texts:
            raise RuntimeError('續談缺少第二輪使用者指令')
    events = [json.loads(line[5:].lstrip()) for line in saved[2]['provider_response'].splitlines() if line.startswith('data:') and line[5:].strip() not in ('', '[DONE]')]
    if any(event.get('type') == 'response.output_item.done' and event.get('item', {}).get('type') == 'function_call' or event.get('type') == 'content_block_start' and event.get('content_block', {}).get('type') == 'tool_use' for event in events):
        raise RuntimeError('續談重新呼叫工具，未只引用既有結果')


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


def validate_error_recovery(saved, payloads, recovery_stream, usage_records, records, codex, interrupted=False, upstream_request_id=None):
    if len(saved) != 2 or len(payloads) != 1 or len(records) != 2 or len(usage_records) != 2 or len({row['capture_id'] for row in records}) != 2:
        raise RuntimeError('錯誤與恢復不是兩個獨立請求')
    if interrupted:
        if [row.get('state') for row in records] != ['incomplete', 'complete']:
            raise RuntimeError('中斷與恢復的保存狀態不符')
        events = [json.loads(line[5:].lstrip()) for line in payloads[0].splitlines() if line.startswith('data:')]
        deltas = [event.get('delta') if event.get('type') == 'response.output_text.delta' else (event.get('delta') or {}).get('text') for event in events if event.get('type') in ('response.output_text.delta', 'content_block_delta')]
        if not any(isinstance(text, str) and text for text in deltas) or any(event.get('type') in ('message_stop', 'response.completed', 'response.failed', 'response.incomplete', 'error') for event in events):
            raise RuntimeError('中斷前未產生部分文字或已有結束事件')
    validate_errors(saved[:1], payloads)
    validate_passthrough_stream(recovery_stream, saved[1]['provider_response'])
    for index, failed in enumerate((True, False)):
        selected = [row for row in usage_records if row['id'] == records[index]['capture_id']]
        validate_usage(selected, records[index:index + 1], interrupted and failed, codex, failed and not interrupted)
        expected_id = upstream_request_id or (json.loads(payloads[0])['request_id'] if failed else None)
        if failed and selected[0]['provider_request_id'] != expected_id:
            raise RuntimeError('上游錯誤編號與嘗試紀錄不一致')
        if selected[0]['status'] != ('failed' if failed else 'completed'):
            raise RuntimeError('失敗或恢復請求的結帳狀態不符')
        if interrupted and failed and selected[0].get('error_type') != 'upstream_body_error':
            raise RuntimeError('串流中斷缺少正確失敗原因')
