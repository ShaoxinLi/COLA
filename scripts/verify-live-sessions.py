"""Drive the built COLA executable across fresh processes and real providers."""
import argparse
import getpass
import hashlib
import http.server
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.request

ENDPOINTS = {
    'deepseek': 'https://api.deepseek.com/responses',
    'bailian': 'https://dashscope.aliyuncs.com/compatible-mode/v1/responses',
}


def encode(value):
    return json.dumps(value, ensure_ascii=False, separators=(',', ':')).encode()


def request_json(url, body=None):
    request = urllib.request.Request(url, data=None if body is None else encode(body),
                                     headers={'Content-Type': 'application/json'})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request, timeout=240) as response:
        return response.status, json.load(response)


def check_archive(folder, calls, session_id):
    paths = list((folder / 'sessions').glob('*.jsonl'))
    assert len(paths) == 1, 'Expected exactly one durable session archive'
    records = [json.loads(line) for line in paths[0].read_text().splitlines()]
    header = records[0]
    assert header['version'] == 2 and header['session_id'] == session_id
    assert header['closed_turn_count'] == 7 and not header['active']
    assert len(header['views']) == 7
    invocations = header['invocations']
    assert len(invocations) == len(calls), 'Every HTTP call must have durable invocation evidence'
    turns = {record['turn']['id']: record['turn'] for record in records[1:]}
    aliases = {'deepseek-flash', 'qwen3.8-max', 'qwen3.8-flash'}
    native_calls = 0
    for index, (invocation, observed) in enumerate(zip(invocations, calls)):
        finished = invocation['state']['Finished']
        request = finished['request']
        raw_request = bytes(request['body']['bytes'])
        body = json.loads(raw_request)
        assert hashlib.sha256(raw_request).hexdigest() == observed['request_sha256']
        assert body == json.loads((folder / f'upstream-{index:02}.request.json').read_text())
        assert len(raw_request) <= 64 * 1024
        assert body['store'] is False and 'previous_response_id' not in body
        assert invocation['requested']['settings']['model_id'] == observed['requested_model'] == body['model']
        response = finished['response']
        raw_response = json.loads(bytes(response['response']['bytes']))
        assert raw_response == json.loads((folder / f'upstream-{index:02}.response.json').read_text())
        assert response['reported_model'] == observed['reported_model']
        assert response['http_status'] == observed['status'] == 200
        assert response['capture'] == 'Complete'
        target = invocation['target']
        assert ('Reviewer' in target) == observed['reviewer']
        if 'Assistant' in target:
            address = target['Assistant']
            assert finished['outcome'] == 'AssistantAccepted'
            step = turns[address['turn']]['steps'][address['step'] - 1]
            emitted = [item['call_id'] for item in raw_response['output'] if item.get('type') == 'function_call']
            if emitted:
                native_calls += len(emitted)
                assert emitted == [call['id'] for call in step['assistant']['calls']]
                assert emitted == [execution['call_id'] for execution in step['executions']]
            if address['step'] == 1:
                assert not any(item.get('type') in {'function_call', 'function_call_output', 'reasoning'} for item in body['input'])
                semantic = json.dumps(body['input'])
                assert not any(alias in semantic for alias in aliases), 'Historical model audit leaked into semantic input'
        else:
            assert finished['outcome']['Review']['allow'] is True
            assert 'CURRENT value' not in body.get('instructions', '')
            review_data = json.dumps(body['input'])
            assert 'CURRENT value' in review_data and 'current.txt' in review_data
            assert target['Reviewer']['call']['native'] in review_data
    assert native_calls >= 4, 'Each read turn must preserve a native call ID'
    return {'invocations': len(invocations), 'closed_turns': 7, 'native_calls': native_calls,
            'exact_request_bytes_match': True, 'response_evidence_matches': True,
            'historic_model_audit_omitted': True, 'old_native_items_omitted_at_turn_start': True}


def stop_child(child):
    if child is None:
        return
    if child.poll() is None:
        child.terminate()
    try:
        child.wait(timeout=5)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait(timeout=5)


def main():
    if not __debug__:
        raise SystemExit('Verification requires assertions. Run Python without -O or PYTHONOPTIMIZE.')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    folder = args.evidence.resolve()
    folder.mkdir(mode=0o700, parents=True, exist_ok=False)
    fixture = folder / 'tool-cwd'
    fixture.mkdir()
    store = folder / 'sessions'
    keys = {'deepseek': getpass.getpass('DeepSeek key (memory only): '),
            'bailian': getpass.getpass('Bailian key (memory only): ')}
    assert all(keys.values()), 'Both provider keys are required'
    rows, calls, faults = [], [], []
    mutex = threading.Lock()

    def save(name, value):
        content = encode(value)
        assert not any(key.encode() in content for key in keys.values()), 'Credential in capture'
        (folder / name).write_bytes(content + b'\n')

    class Broker(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            try:
                provider, label, route = self.path.strip('/').split('/')
                assert route == 'responses'
                body = self.rfile.read(int(self.headers['Content-Length']))
                with mutex:
                    index = len(calls)
                    calls.append({'index': index, 'provider': provider, 'turn': label})
                stem = f'upstream-{index:02}'
                value = json.loads(body)
                save(stem + '.request.json', value)
                upstream = urllib.request.Request(ENDPOINTS[provider], data=body,
                    headers={'Content-Type': 'application/json', 'Authorization': 'Bearer ' + keys[provider]})
                opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                try:
                    with opener.open(upstream, timeout=180) as response:
                        status, raw = response.status, response.read(8 * 1024 * 1024 + 1)
                except urllib.error.HTTPError as error:
                    status, raw = error.code, error.read(8 * 1024 * 1024 + 1)
                assert len(raw) <= 8 * 1024 * 1024, 'Capture limit exceeded'
                response = json.loads(raw)
                save(stem + '.response.json', response)
                with mutex:
                    calls[index].update(status=status, generation_status=response.get('status'),
                        requested_model=value['model'], reported_model=response.get('model'),
                        reviewer='format' in value.get('text', {}),
                        request_sha256=hashlib.sha256(body).hexdigest())
                self.send_response(status)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)
            except Exception as error:
                with mutex:
                    faults.append(type(error).__name__)
                self.send_error(502, 'Verification broker failed')

    broker = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Broker)
    thread = threading.Thread(target=broker.serve_forever, daemon=True)
    thread.start()
    codes, session_id = [], None
    web = None
    cli = None

    def config(turn, provider, model):
        value = {'protocol': 'openai-responses',
            'base_url': f'http://127.0.0.1:{broker.server_port}/{provider}/turn-{turn}',
            'api_key': 'verification-broker-placeholder',
            'assistant': {'model': model, 'max_output_tokens': 8192},
            'reviewer': {'model': model, 'max_output_tokens': 8192},
            'tool_cwd': str(fixture), 'store_root': str(store), 'timeout_ms': 200000,
            'max_iterations': 5}
        if provider == 'bailian':
            value['reviewer']['effort'] = 'off'
        path = folder / f'config-{turn}.json'
        save(path.name, value)
        return path

    def prompt(read):
        if read:
            code = 'LIVE-' + secrets.token_hex(8)
            codes.append(code)
            (fixture / 'current.txt').write_text(code + '\n')
            return ('Call read_file on current.txt exactly once for its CURRENT value. '
                'The file has changed since previous turns. After the tool result, answer with '
                'every earlier LIVE- code in chronological order, followed by the current code. '
                'Do not use bash or write files. Do not guess a value.')
        return ('Use only this session history. Return every previously observed LIVE- code '
                'in chronological order. Do not call tools or read files.')

    def check(turn, provider, model, pid, replies, read):
        errors = [reply for reply in replies if reply.get('kind') == 'error' or 'error' in reply]
        assert not errors, f'COLA failed at turn {turn}: {encode(errors).decode()}'
        result = next(reply for reply in reversed(replies) if 'messages' in reply)
        proposed = [call for message in result['messages']
                    if message.get('kind') == 'assistant_tool_requests' for call in message['calls']]
        returned = [message for message in result['messages'] if message.get('kind') == 'tool_result']
        assert len(proposed) == len(returned) == int(read), 'Expected one read per read turn and no recall tools'
        if read:
            assert proposed[0]['name'] == 'read_file' and proposed[0]['arguments'] == {'path': 'current.txt'}
            assert returned[0]['outcome']['kind'] == 'success'
            assert returned[0]['outcome']['text'].strip() == codes[-1]
        answers = [message['text'] for message in result['messages']
                   if message.get('kind') == 'assistant_answer']
        assert answers, 'No final assistant answer'
        answer = answers[-1]
        cursor = 0
        for code in codes:
            position = answer.find(code, cursor)
            assert position >= 0, f'Ordered historical code missing at turn {turn}'
            cursor = position + len(code)
        current = [call for call in calls if call['turn'] == f'turn-{turn}']
        assert any(call['reviewer'] for call in current) == read, 'Unexpected reviewer/tool behavior'
        assert all(call['status'] == 200 and call['generation_status'] == 'completed' for call in current)
        row = {'turn': turn, 'provider': provider, 'model': model, 'pid': pid,
               'read_current_fixture': read, 'verified_codes': len(codes), 'upstream_requests': len(current)}
        rows.append(row)
        save(f'turn-{turn}.replies.json', replies)
        save('summary.json', {'complete': False, 'session_id': session_id, 'turns': rows, 'requests': calls})
        print(json.dumps(row), flush=True)

    targets = [('deepseek', 'deepseek-flash', True), ('bailian', 'qwen3.8-max', True),
               ('bailian', 'qwen3.8-flash', True), ('deepseek', 'deepseek-flash', False),
               ('bailian', 'qwen3.8-max', False)]
    try:
        print('Evidence: ' + str(folder), flush=True)
        save('binary.json', {'path': str(binary), 'sha256': hashlib.sha256(binary.read_bytes()).hexdigest()})
        for turn, (provider, model, read) in enumerate(targets, 1):
            path = config(turn, provider, model)
            text = prompt(read)
            lines = ['/new' if session_id is None else '/use ' + session_id, text, '/exit']
            cli = subprocess.Popen([str(binary), 'cli', '--config', str(path)],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            stdout, stderr = cli.communicate('\n'.join(lines) + '\n', timeout=480)
            exit_code, pid = cli.returncode, cli.pid
            cli = None
            (folder / f'turn-{turn}.stderr').write_text(stderr)
            replies = [json.loads(line) for line in stdout.splitlines()]
            save(f'turn-{turn}.replies.json', replies)
            if session_id is None:
                session_id = next(reply['session']['id'] for reply in replies if reply['kind'] == 'created')
            assert exit_code == 0, f'CLI exit {exit_code}, inspect turn-{turn}.replies.json'
            check(turn, provider, model, pid, replies, read)
        for turn, (provider, model, read) in enumerate([
                ('deepseek', 'deepseek-flash', True), ('bailian', 'qwen3.8-max', False)], 6):
            path = config(turn, provider, model)
            with socket.socket() as sock:
                sock.bind(('127.0.0.1', 0))
                port = sock.getsockname()[1]
            base = f'http://127.0.0.1:{port}'
            log = (folder / f'web-{turn}.log').open('w')
            web = subprocess.Popen([str(binary), 'web', '--config', str(path), '--bind', f'127.0.0.1:{port}'],
                                   stdout=log, stderr=log)
            for _ in range(100):
                assert web.poll() is None, 'Web process exited during startup'
                try:
                    request_json(base + '/sessions')
                    break
                except (OSError, urllib.error.URLError):
                    time.sleep(0.05)
            else:
                raise AssertionError('Web readiness timeout')
            status, result = request_json(base + '/sessions/' + session_id + '/messages', {'text': prompt(read)})
            assert status == 200
            check(turn, provider, model, web.pid, [result], read)
            stop_child(web)
            web = None
            log.close()
        assert len(rows) == 7, 'Expected seven completed COLA process runs'
        assert not faults, 'Broker faults: ' + repr(faults)
        save('archive-verification.json', check_archive(folder, calls, session_id))
        for file in folder.rglob('*'):
            if file.is_file():
                raw = file.read_bytes()
                assert not any(key.encode() in raw for key in keys.values()), 'Credential persisted'
        save('summary.json', {'complete': True, 'session_id': session_id, 'turns': rows, 'requests': calls})
        print('VERIFIED: seven fresh COLA processes, one durable session, two providers, three models.', flush=True)
    finally:
        stop_child(cli)
        stop_child(web)
        broker.shutdown()
        broker.server_close()
        thread.join(timeout=5)


if __name__ == '__main__':
    main()
