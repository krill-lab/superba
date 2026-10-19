#!/usr/bin/env python3
"""Real coding clients through forced cross-protocol router paths (Linux).

SUPERBA_E2E_ENV supplies SUPERBA_E2E_UPSTREAM and SUPERBA_E2E_KEY. Install
Gemini CLI into a temporary npm prefix and pass --gemini. Every client gets
an empty HOME/config directory and a scratch project. No production client
configuration is read. Reports omit credentials and upstream addresses.

The upstream member exposes exactly one native protocol in each scenario:
Claude Code -> Responses and Chat, Codex -> Chat, Gemini CLI -> Chat, and
an HTTP Responses request -> the relay's Gemini v1beta API. A random file
marker, absent from prompts, proves the three CLIs executed a tool and
continued the conversation with its result.
"""
import argparse
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import threading
import hashlib
import hmac
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import sqlite3
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


def load_env(path):
    values = {}
    for line in Path(path).read_text().splitlines():
        if line.strip() and not line.lstrip().startswith('#') and '=' in line:
            key, value = line.split('=', 1)
            values[key.strip()] = value.strip().strip('"').strip("'")
    return values


def token(root, harness):
    source = f'h:{harness}'.encode()
    digest = hmac.new(root.encode(), b'superba-router-source-v1\0' + source, hashlib.sha256).hexdigest()
    return f'sbs_{source.hex()}_{digest}'


def write_private(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), 'w') as f:
        f.write(data)


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('out')
    ap.add_argument('--router', required=True)
    ap.add_argument('--claude', default=shutil.which('claude'))
    ap.add_argument('--codex', default=shutil.which('codex'))
    ap.add_argument('--gemini', required=True)
    ap.add_argument('--model', default='gpt-5.6-luna')
    ap.add_argument('--gemini-upstream-mode', choices=['real', 'fake'], default='real')
    ap.add_argument('--scenario', action='append', choices=['claude-responses', 'claude-chat', 'codex-chat', 'gemini-chat', 'gemini-upstream'])
    a = ap.parse_args()
    credentials = load_env(os.environ['SUPERBA_E2E_ENV'])
    key, upstream = credentials['SUPERBA_E2E_KEY'], credentials['SUPERBA_E2E_UPSTREAM'].rstrip('/')
    work = Path(a.out).resolve()
    work.mkdir(parents=True, exist_ok=True)
    report = {'model': a.model, 'gemini_upstream': a.gemini_upstream_mode, 'runs': [], 'checks': []}
    path = ':'.join(dict.fromkeys([str(Path(shutil.which('node')).parent), '/usr/local/bin', '/usr/bin', '/bin']))
    secrets = [key, upstream, urllib.parse.urlsplit(upstream).hostname]
    secrets = [value for value in secrets if value]

    def clean(value):
        for secret in sorted(secrets, key=len, reverse=True):
            value = value.replace(secret, '<redacted>')
        return value

    def check(ok, name):
        report['checks'].append({'ok': bool(ok), 'what': name})
        print(('PASS ' if ok else 'FAIL ') + name, flush=True)

    for scenario in a.scenario or ['claude-responses', 'claude-chat', 'codex-chat', 'gemini-chat', 'gemini-upstream']:
        case = work / scenario
        case.mkdir(exist_ok=True)
        home, project, data = case / 'home', case / 'project', case / 'data'
        for directory in [home, project, data / 'router', home / 'claude', home / 'codex', home / '.gemini']:
            directory.mkdir(parents=True, exist_ok=True)
        marker = 'TR_' + uuid.uuid4().hex
        (project / 'marker.txt').write_text(marker + '\n')
        root = 'sbr_' + uuid.uuid4().hex + uuid.uuid4().hex
        secrets.append(root)
        harness = 'claude-code' if scenario.startswith('claude') else ('gemini' if scenario.startswith('gemini') else 'codex')
        bearer = token(root, harness)
        secrets.append(bearer)
        port = free_port()
        native = scenario.split('-')[-1] if scenario != 'gemini-upstream' else 'gemini'
        model_list = [{'id': a.model}]
        native_server, native_seen = None, []
        relay_base, member_key = upstream, key
        if scenario == 'gemini-upstream' and a.gemini_upstream_mode == 'fake':
            class NativeGemini(BaseHTTPRequestHandler):
                def do_POST(self):
                    body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    native_seen.append({'path': self.path, 'has_contents': bool(body.get('contents')),
                                        'auth_ok': self.headers.get('x-goog-api-key') == 'gemini-fixture-key',
                                        'no_envelope_fields': 'model' not in body and 'stream' not in body})
                    answer = json.dumps({'responseId': 'gemini-fixture', 'modelVersion': a.model,
                        'candidates': [{'content': {'role': 'model', 'parts': [{'text': 'GEMINI_UPSTREAM_OK'}]}, 'finishReason': 'STOP'}],
                        'usageMetadata': {'promptTokenCount': 12, 'candidatesTokenCount': 6, 'totalTokenCount': 18}}).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                    self.send_header('Content-Length', str(len(answer)))
                    self.end_headers()
                    self.wfile.write(answer)
                def log_message(self, *_args):
                    pass
            native_server = ThreadingHTTPServer(('127.0.0.1', 0), NativeGemini)
            threading.Thread(target=native_server.serve_forever, daemon=True).start()
            relay_base, member_key = f'http://127.0.0.1:{native_server.server_port}', 'gemini-fixture-key'

        cfg = {'format': 1, 'listen_port': port, 'use_system_proxy': False,
               'members': [{'id': 'relay', 'name': 'Test relay', 'kind': 'compatible', 'websocket': False,
                            'lines': [{'id': 'main', 'name': 'Main', 'base_url': relay_base + '/codex/v1'}]}],
               'tuning': {'active_probes': False},
               'stats': {'record': True, 'line': None},
               'claude': {'members': {'relay': {'models': model_list}}},
               'openai': {'members': {'relay': {'models': model_list, **({native: {}} if native != 'gemini' else {})}}},
               'gemini': {'members': {'relay': {'models': model_list, **({'endpoint': {'path': '/v1beta'}} if native == 'gemini' else {})}}}}
        write_private(data / 'router' / 'config.json', json.dumps(cfg))
        write_private(data / 'router' / 'secrets.json', json.dumps({'format': 1, 'router_token': root, 'member_keys': {'relay': member_key}}))
        env = {'PATH': path, 'HOME': str(home), 'LANG': 'C.UTF-8', 'TERM': 'dumb',
               'XDG_CONFIG_HOME': str(home / '.config'), 'XDG_DATA_HOME': str(home / '.local/share'),
               'XDG_CACHE_HOME': str(home / '.cache'), 'NO_COLOR': '1'}
        rlog = open(case / 'router.stderr', 'w')
        router = subprocess.Popen([a.router, 'run', '--data-dir', str(data)], env={**env, 'RUST_LOG': 'info'},
                                  stdout=rlog, stderr=rlog, start_new_session=True)
        start = time.monotonic()
        try:
            for _ in range(100):
                if (data / 'router/runtime.json').exists() or router.poll() is not None:
                    break
                time.sleep(0.1)
            if router.poll() is not None:
                raise RuntimeError('router exited before startup')
            prompt = 'Use your file-reading or shell tool to read marker.txt in the current directory. Reply with only its exact contents. Do not guess.'
            if scenario.startswith('claude'):
                env.update({'CLAUDE_CONFIG_DIR': str(home / 'claude'), 'ANTHROPIC_BASE_URL': f'http://127.0.0.1:{port}/claude',
                            'ANTHROPIC_AUTH_TOKEN': bearer, 'ANTHROPIC_MODEL': a.model,
                            'ANTHROPIC_DEFAULT_OPUS_MODEL': a.model, 'ANTHROPIC_DEFAULT_SONNET_MODEL': a.model,
                            'ANTHROPIC_DEFAULT_HAIKU_MODEL': a.model, 'CLAUDE_CODE_GATEWAY_HINT_HEADERS': '1',
                            'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC': '1', 'DISABLE_AUTOUPDATER': '1',
                            'DISABLE_TELEMETRY': '1', 'DISABLE_ERROR_REPORTING': '1'})
                command = [a.claude, '-p', prompt, '--model', a.model, '--tools', 'Read', '--allowedTools', 'Read', '--output-format', 'stream-json', '--verbose']
            elif scenario == 'codex-chat':
                env.update({'CODEX_HOME': str(home / 'codex'), 'TR_ROUTER_TOKEN': bearer})
                config = f'''model_provider = "tr"
model = "{a.model}"
approval_policy = "never"
sandbox_mode = "read-only"
web_search = "disabled"
[model_providers.tr]
name = "Translation E2E"
base_url = "http://127.0.0.1:{port}/backend-api/codex"
wire_api = "responses"
env_key = "TR_ROUTER_TOKEN"
supports_websockets = false
'''
                write_private(home / 'codex/config.toml', config)
                command = [a.codex, 'exec', '--ephemeral', '--skip-git-repo-check', '--json', prompt]
            elif scenario == 'gemini-chat':
                env.update({'GEMINI_API_KEY': bearer, 'GOOGLE_GEMINI_BASE_URL': f'http://127.0.0.1:{port}/gemini',
                            'GOOGLE_GENAI_API_VERSION': 'v1beta', 'GEMINI_CLI_NO_RELAUNCH': 'true'})
                write_private(home / '.gemini/settings.json', json.dumps({'security': {'auth': {'selectedType': 'gemini-api-key'}},
                              'telemetry': {'enabled': False}, 'general': {'disableAutoUpdate': True}}))
                command = [a.gemini, '-m', a.model, '-p', prompt, '--approval-mode', 'plan', '--skip-trust', '--output-format', 'stream-json']
            else:
                req = urllib.request.Request(f'http://127.0.0.1:{port}/openai/v1/responses',
                       data=json.dumps({'model': a.model, 'stream': False, 'input': 'Reply with only GEMINI_UPSTREAM_OK.'}).encode(),
                       headers={'Authorization': f'Bearer {bearer}', 'Content-Type': 'application/json'})
                with urllib.request.urlopen(req, timeout=180) as response:
                    output = response.read().decode()
                write_private(case / 'http.json', clean(output))
                check('GEMINI_UPSTREAM_OK' in output, scenario + ': native Gemini response converted to Responses')
                continue
            p = subprocess.run(command, cwd=project, env=env, capture_output=True, text=True, timeout=240, stdin=subprocess.DEVNULL)
            write_private(case / 'client.stdout', clean(p.stdout))
            write_private(case / 'client.stderr', clean(p.stderr))
            check(p.returncode == 0, scenario + ': CLI exits successfully')
            events = []
            for line in p.stdout.splitlines():
                try:
                    events.append(json.loads(line))
                except ValueError:
                    pass
            answer = ''.join(v.get('content', '') for v in events if v.get('type') == 'message' and v.get('role') == 'assistant')
            check(marker in p.stdout or marker in answer, scenario + ': tool read a marker absent from the prompt')
            report['runs'].append({'scenario': scenario, 'exit': p.returncode, 'duration_s': round(time.monotonic() - start, 2)})
        except (RuntimeError, subprocess.TimeoutExpired, urllib.error.URLError) as error:
            if isinstance(error, urllib.error.HTTPError):
                write_private(case / 'http-error.json', clean(error.read().decode(errors='replace')))
            check(False, scenario + ': ' + clean(str(error)))
            if isinstance(error, subprocess.TimeoutExpired):
                for field in ['stdout', 'stderr']:
                    content = getattr(error, field, None) or b''
                    if isinstance(content, bytes):
                        content = content.decode(errors='replace')
                    write_private(case / f'client.{field}', clean(content))
        finally:
            time.sleep(0.6)
            if router.poll() is None:
                os.killpg(router.pid, signal.SIGTERM)
                try:
                    router.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(router.pid, signal.SIGKILL)
                    router.wait()
            rlog.close()
            log = case / 'router.stderr'
            log.write_text(clean(log.read_text(errors='replace')))
            dbpath = data / 'router/usage.sqlite3'
            if dbpath.exists():
                with closing(sqlite3.connect(f'file:{dbpath}?mode=ro', uri=True)) as db:
                    db.row_factory = sqlite3.Row
                    rows = [dict(r) for r in db.execute('SELECT harness,protocol,model,status_kind,http_status,input_tokens,output_tokens FROM requests ORDER BY id')]
                write_private(case / 'requests.json', clean(json.dumps(rows, indent=2)))
                # The database (and WAL) contains upstream_host and is not
                # part of the evidence. Keep only the sanitized projection.
                for database_file in dbpath.parent.glob('usage.sqlite3*'):
                    database_file.unlink(missing_ok=True)
                expected = 'claude_code' if harness == 'claude-code' else harness
                check(bool(rows) and all(r['harness'] == expected for r in rows), scenario + ': router records the correct harness')
                check(any(r['status_kind'] == 'ok' and r['input_tokens'] and r['output_tokens'] for r in rows), scenario + ': normalized usage is recorded')
            if native_server:
                native_server.shutdown()
                native_server.server_close()
                check(len(native_seen) == 1 and all(r['has_contents'] and r['auth_ok'] and r['no_envelope_fields'] for r in native_seen),
                      scenario + ': fake upstream received native Gemini fields and member credentials')
                write_private(case / 'native-upstream.json', json.dumps(native_seen, indent=2))
            (data / 'router/secrets.json').unlink(missing_ok=True)
            (data / 'router/config.json').unlink(missing_ok=True)
            shutil.rmtree(home)
            write_private(work / 'report.json', json.dumps(report, indent=2))
    return 0 if all(c['ok'] for c in report['checks']) else 1


if __name__ == '__main__':
    raise SystemExit(main())
