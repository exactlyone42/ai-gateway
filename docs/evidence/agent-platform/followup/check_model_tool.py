import json
from pathlib import Path
import subprocess
import time
import urllib.request

out = Path(__file__).resolve().parent
root = out.parents[3]
processes = subprocess.check_output(['ps', '-eo', 'comm'], text=True).lower()
if 'godot' in processes or 'unity' in processes:
    raise SystemExit('Rendering process present; local inference deferred')
subprocess.run(['/usr/local/libexec/ollama-memory-gate'], check=True, capture_output=True)
model = 'huihui_ai/Qwen3.8-abliterated:latest'
tool = {'type': 'function', 'function': {'name': 'read_document', 'description': 'Read the first line of the permitted architecture document.', 'parameters': {'type': 'object', 'properties': {'path': {'type': 'string'}}, 'required': ['path']}}}
messages = [{'role': 'user', 'content': 'Use read_document to read docs/architecture.md. Do not guess. After the tool responds, output JSON with key first_line copied exactly from the tool result.'}]
results = []
started = time.monotonic()
checks = {}
try:
    for step in range(2):
        payload = {'model': model, 'stream': False, 'think': False, 'messages': messages, 'tools': [tool], 'options': {'num_ctx': 4096, 'num_predict': 160}}
        request = urllib.request.Request('http://127.0.0.1:11434/api/chat', data=json.dumps(payload).encode(), headers={'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=45) as response:
            data = json.load(response)
        results.append(data)
        message = data['message']
        messages.append(message)
        if step == 0:
            calls = message.get('tool_calls', [])
            checks['one_tool_call'] = len(calls) == 1
            if len(calls) != 1:
                break
            function = calls[0]['function']
            checks['correct_name_and_path'] = function['name'] == 'read_document' and function['arguments'] == {'path': 'docs/architecture.md'}
            if not checks['correct_name_and_path']:
                break
            first_line = (root / 'docs/architecture.md').read_text().splitlines()[0]
            messages.append({'role': 'tool', 'tool_name': 'read_document', 'content': json.dumps({'first_line': first_line}, ensure_ascii=False)})
        else:
            content = message.get('content', '').strip()
            try:
                checks['answer_matches_file'] = json.loads(content) == {'first_line': first_line}
            except ValueError:
                checks['answer_matches_file'] = False
except Exception as exc:
    checks['error'] = str(exc)
result = {'model': model, 'seconds': round(time.monotonic()-started,3), 'checks': checks, 'responses': results, 'scope': 'Local Ollama tool call and one permitted read-only file; no Gateway request'}
(out/'model-tool-results.json').write_text(json.dumps(result,ensure_ascii=False,indent=2))
print(json.dumps({'checks':checks,'seconds':result['seconds']},ensure_ascii=False))
