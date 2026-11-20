#!/usr/bin/env python3
import argparse
import json
import os
import re
import subprocess
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODELS = ['claude-sonnet-5-5', 'claude-opus-5-5', 'claude-haiku-4-5-20251001']
CLAUDE_BIN = os.environ.get('CLAUDE_BIN', 'claude')
WORK_DIR = os.environ.get('CLAUDE_BRIDGE_CWD', os.path.expanduser('~/.cache/claude-bridge-cwd'))
TIMEOUT_SECONDS = int(os.environ.get('CLAUDE_BRIDGE_TIMEOUT', '1800'))
KEEPALIVE_SECONDS = float(os.environ.get('CLAUDE_BRIDGE_KEEPALIVE', '15'))
SLOTS = threading.BoundedSemaphore(int(os.environ.get('CLAUDE_BRIDGE_CONCURRENCY', '2')))
UNSUPPORTED_FIELDS = ('functions', 'function_call')
TOOL_BLOCK = re.compile(r'<tool_calls>(.*?)</tool_calls>', re.S)


class BridgeError(Exception):
    def __init__(self, status, code, message):
        super().__init__(message)
        self.status = status
        self.code = code


def text_of(content):
    if content is None:
        return ''
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for part in content:
            if not isinstance(part, dict) or part.get('type') not in ('text', 'input_text'):
                raise BridgeError(400, 'unsupported_content', '只支援文字內容')
            parts.append(part.get('text', ''))
        return '\n'.join(parts)
    raise BridgeError(400, 'invalid_request', 'message content 格式不正確')


def tool_specs(body):
    if body.get('tool_choice') == 'none':
        return []
    specs = []
    for tool in body.get('tools') or []:
        function = tool.get('function') if isinstance(tool, dict) else None
        if not isinstance(function, dict) or not function.get('name'):
            raise BridgeError(400, 'invalid_request', 'tools 格式不正確')
        specs.append(function)
    return specs


def tool_instructions(specs, choice):
    lines = ['You can call the tools below. Tool definitions (JSON):', json.dumps(specs, ensure_ascii=False),
             'To call tools, reply with only this block and nothing else:',
             '<tool_calls>[{"name": "<tool name>", "arguments": {<arguments as a JSON object>}}]</tool_calls>',
             'Put several objects in the array to call several tools at once.',
             'Tool results come back in later messages labeled [tool result]. When you have enough information, answer in plain text.']
    if choice == 'required':
        lines.append('You must call at least one tool in this reply.')
    elif isinstance(choice, dict) and (choice.get('function') or {}).get('name'):
        lines.append('You must call the tool ' + choice['function']['name'] + ' in this reply.')
    return '\n'.join(lines)


def render_calls(message, names_by_id):
    calls = []
    for call in message.get('tool_calls') or []:
        function = call.get('function') or {}
        raw = function.get('arguments')
        try:
            arguments = json.loads(raw) if isinstance(raw, str) else (raw or {})
        except ValueError:
            arguments = raw
        names_by_id[call.get('id')] = function.get('name')
        calls.append({'name': function.get('name'), 'arguments': arguments})
    return '<tool_calls>' + json.dumps(calls, ensure_ascii=False) + '</tool_calls>' if calls else ''


def build_prompt(body):
    for field in UNSUPPORTED_FIELDS:
        if body.get(field):
            raise BridgeError(400, 'unsupported_feature', field + ' 不支援，請改用 tools／tool_choice')
    specs = tool_specs(body)
    system, turns, names_by_id = [], [], {}
    for message in body.get('messages') or []:
        role, text = message.get('role'), text_of(message.get('content'))
        if role in ('system', 'developer'):
            system.append(text)
        elif role == 'user':
            turns.append((role, text))
        elif role == 'assistant':
            turns.append((role, '\n'.join(part for part in (text, render_calls(message, names_by_id)) if part)))
        elif role == 'tool':
            identifier = message.get('tool_call_id')
            turns.append(('tool result', f'name={names_by_id.get(identifier, "")} id={identifier}\n{text}'))
        else:
            raise BridgeError(400, 'unsupported_role', '不支援的 role：' + str(role))
    if not turns or turns[-1][0] == 'assistant':
        raise BridgeError(400, 'invalid_request', '最後一則訊息必須是 user 或 tool')
    if len(turns) == 1 and turns[0][0] == 'user':
        prompt = turns[0][1]
    else:
        lines = ['Conversation so far. Continue as the assistant by replying to the last message.', '']
        lines += [f'[{role}]\n{text}\n' for role, text in turns]
        prompt = '\n'.join(lines)
    if specs:
        system.append(tool_instructions(specs, body.get('tool_choice')))
    return '\n\n'.join(system) or 'You are a helpful assistant.', prompt


def extract_tool_calls(text, names):
    match = TOOL_BLOCK.search(text)
    if not match:
        return text, []
    try:
        raw = json.loads(match.group(1))
    except ValueError:
        return text, []
    raw = [raw] if isinstance(raw, dict) else raw
    calls = []
    for item in raw if isinstance(raw, list) else []:
        if not isinstance(item, dict) or item.get('name') not in names:
            return text, []
        arguments = item.get('arguments', {})
        calls.append({'id': 'call_' + uuid.uuid4().hex[:24], 'type': 'function',
                      'function': {'name': item['name'],
                                   'arguments': arguments if isinstance(arguments, str) else json.dumps(arguments, ensure_ascii=False)}})
    if not calls:
        return text, []
    return (text[:match.start()] + text[match.end():]).strip(), calls


def command(model, system, stream):
    args = [CLAUDE_BIN, '-p', '--model', model, '--tools', '', '--system-prompt', system,
            '--disable-slash-commands', '--no-session-persistence', '--setting-sources', '',
            '--strict-mcp-config', '--output-format', 'stream-json' if stream else 'json']
    if stream:
        args += ['--verbose', '--include-partial-messages']
    return args


def start(model, system, prompt, stream):
    os.makedirs(WORK_DIR, exist_ok=True)
    process = subprocess.Popen(command(model, system, stream), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, cwd=WORK_DIR, text=True)
    if stream:
        process.stdin.write(prompt)
        process.stdin.close()
    return process


def usage_of(raw):
    usage = raw.get('usage') or {}
    prompt = sum(usage.get(key, 0) or 0 for key in ('input_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens'))
    completion = usage.get('output_tokens', 0) or 0
    return {'prompt_tokens': prompt, 'completion_tokens': completion, 'total_tokens': prompt + completion}


def failure(result_text):
    lowered = (result_text or '').lower()
    if 'rate' in lowered and 'limit' in lowered or 'usage limit' in lowered:
        return BridgeError(429, 'rate_limit_error', result_text)
    return BridgeError(502, 'claude_code_error', result_text or 'claude 執行失敗')


def complete(model, system, prompt):
    process = start(model, system, prompt, False)
    try:
        out, err = process.communicate(prompt, timeout=TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        process.communicate()
        raise BridgeError(504, 'timeout', 'claude 執行逾時')
    try:
        raw = json.loads(out)
    except ValueError:
        raise BridgeError(502, 'claude_code_error', (err or out or 'claude 沒有輸出').strip()[:300])
    if process.returncode != 0 or raw.get('is_error'):
        raise failure(raw.get('result') or err)
    return raw.get('result', ''), usage_of(raw)


def chunk(identifier, created, model, delta, finish=None, usage=None):
    body = {'id': identifier, 'object': 'chat.completion.chunk', 'created': created, 'model': model,
            'choices': [{'index': 0, 'delta': delta, 'finish_reason': finish}]}
    if usage:
        body['usage'] = usage
    return 'data: ' + json.dumps(body, ensure_ascii=False) + '\n\n'


def stream_events(model, system, prompt):
    process = start(model, system, prompt, True)
    deadline = time.monotonic() + TIMEOUT_SECONDS
    finished = False
    try:
        for line in process.stdout:
            if time.monotonic() > deadline:
                process.kill()
                raise BridgeError(504, 'timeout', 'claude 執行逾時')
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if event.get('type') == 'stream_event':
                delta = (event.get('event') or {}).get('delta') or {}
                if delta.get('type') == 'text_delta':
                    yield 'text', delta.get('text', '')
            elif event.get('type') == 'result':
                if event.get('is_error'):
                    raise failure(event.get('result'))
                finished = True
                yield 'usage', usage_of(event)
        process.wait(timeout=10)
        if not finished:
            raise BridgeError(502, 'claude_code_error', (process.stderr.read() or 'claude 沒有輸出結果').strip()[:300])
    finally:
        if process.poll() is None:
            process.kill()
        process.stdout.close()
        process.stderr.close()


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, format, *args):
        return

    def send_json(self, status, body):
        data = json.dumps(body, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def send_error_body(self, error):
        self.send_json(error.status, {'error': {'message': str(error), 'type': error.code, 'code': error.code}})

    def do_GET(self):
        if self.path == '/api/version':
            self.send_json(200, {'version': 'claude-bridge'})
        elif self.path.split('?')[0] in ('/v1/models', '/api/tags'):
            self.send_json(200, {'object': 'list', 'data': [{'id': name, 'object': 'model', 'owned_by': 'claude-code'} for name in MODELS],
                                 'models': [{'name': name, 'model': name} for name in MODELS]})
        else:
            self.send_json(404, {'error': {'message': 'not found', 'type': 'not_found'}})

    def do_POST(self):
        if self.path.split('?')[0] != '/v1/chat/completions':
            self.send_json(404, {'error': {'message': 'not found', 'type': 'not_found'}})
            return
        try:
            body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))))
            model = body.get('model')
            if model not in MODELS:
                raise BridgeError(404, 'model_not_found', '不支援的模型：' + str(model))
            system, prompt = build_prompt(body)
        except ValueError:
            self.send_error_body(BridgeError(400, 'invalid_request', 'JSON 格式不正確'))
            return
        except BridgeError as error:
            self.send_error_body(error)
            return
        if not SLOTS.acquire(timeout=30):
            self.send_error_body(BridgeError(429, 'rate_limit_error', '轉接服務忙碌，請稍後再試'))
            return
        try:
            self.answer(body, model, system, prompt)
        finally:
            SLOTS.release()

    def start_event_stream(self):
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Cache-Control', 'no-cache')
        self.send_header('Connection', 'close')
        self.end_headers()
        self.close_connection = True

    def send_buffered_stream(self, identifier, created, model, text, calls, usage, started=False):
        if not started:
            self.start_event_stream()
        pieces = [chunk(identifier, created, model, {'role': 'assistant', 'content': ''})]
        if text:
            pieces.append(chunk(identifier, created, model, {'content': text}))
        if calls:
            deltas = [{'index': index, **call} for index, call in enumerate(calls)]
            pieces.append(chunk(identifier, created, model, {'tool_calls': deltas}))
        pieces.append(chunk(identifier, created, model, {}, 'tool_calls' if calls else 'stop', usage))
        pieces.append('data: [DONE]\n\n')
        try:
            self.wfile.write(''.join(pieces).encode())
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            return

    def answer(self, body, model, system, prompt):
        identifier, created = 'chatcmpl-' + uuid.uuid4().hex, int(time.time())
        names = {spec['name'] for spec in tool_specs(body)}
        if names or not body.get('stream'):
            streaming = bool(body.get('stream'))
            outcome = {}

            def work():
                try:
                    outcome['value'] = complete(model, system, prompt)
                except BridgeError as error:
                    outcome['error'] = error

            worker = threading.Thread(target=work, daemon=True)
            worker.start()
            started = False
            worker.join(KEEPALIVE_SECONDS if streaming else None)
            while worker.is_alive():
                try:
                    if not started:
                        self.start_event_stream()
                        started = True
                    self.wfile.write(b': keepalive\n\n')
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    worker.join()
                    return
                worker.join(KEEPALIVE_SECONDS)
            if 'error' in outcome:
                error = outcome['error']
                if not started:
                    self.send_error_body(error)
                    return
                try:
                    self.wfile.write(('data: ' + json.dumps({'error': {'message': str(error), 'type': error.code}}) + '\n\ndata: [DONE]\n\n').encode())
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass
                return
            text, usage = outcome['value']
            text, calls = extract_tool_calls(text, names) if names else (text, [])
            if streaming:
                self.send_buffered_stream(identifier, created, model, text, calls, usage, started)
                return
            message = {'role': 'assistant', 'content': text or None} if calls else {'role': 'assistant', 'content': text}
            if calls:
                message['tool_calls'] = calls
            self.send_json(200, {'id': identifier, 'object': 'chat.completion', 'created': created, 'model': model,
                                 'choices': [{'index': 0, 'message': message, 'finish_reason': 'tool_calls' if calls else 'stop'}],
                                 'usage': usage})
            return
        events = stream_events(model, system, prompt)
        try:
            first = next(events, None)
        except BridgeError as error:
            self.send_error_body(error)
            return
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Cache-Control', 'no-cache')
        self.send_header('Connection', 'close')
        self.end_headers()
        self.close_connection = True
        usage = None

        def write(text):
            self.wfile.write(text.encode())
            self.wfile.flush()

        try:
            write(chunk(identifier, created, model, {'role': 'assistant', 'content': ''}))
            pending = [first] if first else []
            for kind, value in [*pending, *events]:
                if kind == 'text':
                    write(chunk(identifier, created, model, {'content': value}))
                else:
                    usage = value
            write(chunk(identifier, created, model, {}, 'stop', usage))
            write('data: [DONE]\n\n')
        except BridgeError as error:
            write('data: ' + json.dumps({'error': {'message': str(error), 'type': error.code}}) + '\n\n')
        except (BrokenPipeError, ConnectionResetError):
            events.close()


def main():
    parser = argparse.ArgumentParser(description='把 Chat Completions 純文字請求轉給本機 claude -p')
    parser.add_argument('--bind', default='172.22.0.1')
    parser.add_argument('--port', type=int, default=11435)
    args = parser.parse_args()
    ThreadingHTTPServer((args.bind, args.port), Handler).serve_forever()


if __name__ == '__main__':
    main()
