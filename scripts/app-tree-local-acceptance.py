#!/usr/bin/env python3
"""Run #230 on a NEW loopback stack; never discover an installed enrollment.

Build the four native binaries from --source, and build its production Web first.
Only fresh task-owned PostgreSQL/process/App identities are created and stopped.
The final JSON is safe to share; private.log and browser-fixture.json are not.
"""
import argparse
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def write(path, value):
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as stream:
        json.dump(value, stream, indent=2)
        stream.flush()
        os.fsync(stream.fileno())


def port():
    with socket.socket() as stream:
        stream.bind(('127.0.0.1', 0))
        return stream.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--native-dir', type=Path, required=True)
    parser.add_argument('--pg-bin', type=Path, required=True)
    parser.add_argument('--web-root', type=Path, required=True)
    parser.add_argument('--python', default='/usr/bin/python3')
    parser.add_argument('--node', default='node')
    args = parser.parse_args()
    os.umask(0o077)
    source, native, web = (p.resolve(strict=True) for p in (args.source, args.native_dir, args.web_root))
    sha = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
    names = ('treer', 'treer-agent-server', 'treer-agent-host', 'treer-proxy')
    pins = {name: digest(native / name) for name in names}
    app_source = source / 'apps/agent-hub/app.py'
    probe_source = source / 'scripts/app-tree-cli-probe.py'
    run = Path(tempfile.mkdtemp(prefix='t230-', dir=str(Path(tempfile.gettempdir()).resolve())))
    for leaf in ('bin', 'home', 'config', 'state', 'runtime', 'workspace'):
        (run / leaf).mkdir(mode=0o700)
    for name in names:
        shutil.copyfile(native / name, run / 'bin' / name)
        (run / 'bin' / name).chmod(0o700)
        if digest(run / 'bin' / name) != pins[name]:
            raise RuntimeError('native_copy_mismatch')
    ports = dict(zip(('postgres', 'proxy', 'controller', 'app'), (port() for _ in range(4))))
    ports['web'] = 54979
    if len(set(ports.values())) != len(ports):
        raise RuntimeError('port_collision')
    for value in ports.values():
        with socket.socket() as stream:
            if stream.connect_ex(('127.0.0.1', value)) == 0:
                raise RuntimeError('requested_port_occupied')
    base = 'http://127.0.0.1:' + str(ports['proxy'])
    origin = 'http://127.0.0.1:54979'
    env = {'PATH': str(run / 'bin') + ':/usr/bin:/bin', 'HOME': str(run / 'home'),
           'XDG_CONFIG_HOME': str(run / 'config'), 'XDG_STATE_HOME': str(run / 'state'),
           'XDG_RUNTIME_DIR': str(run / 'runtime'), 'TREER_STATE_DIR': str(run / 'state'),
           'TREER_RUNTIME_DIR': str(run / 'runtime'), 'TREER_NETWORK_MODE': 'plain',
           'LANG': 'en_US.UTF-8', 'LC_ALL': 'en_US.UTF-8', 'RUST_LOG': 'warn'}
    result = {'source_sha': sha, 'scope': 'fresh real App-owned recursive tree and Core Messages',
              'passed': False, 'stage': 'bootstrap', 'checks': [], 'native_sha256': pins,
              'app_source_sha256': digest(app_source), 'probe_sha256': digest(probe_source),
              'web_sha256': digest(web / 'dist/index.html'),
              'web_runner_sha256': digest(web / 'e2e/app-tree-direct.mjs'), 'installed_mutation': False}
    write(run / 'intent.json', {'source_sha': sha, 'ports': ports, 'private_resources_only': True})
    log = (run / 'private.log').open('xb')
    children, identities, pgstarted = [], set(), False
    ws, app_id, sender_id = None, None, None
    jar = http.cookiejar.CookieJar()
    client = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPCookieProcessor(jar))
    anonymous = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def command(argv, extra=None):
        subprocess.run(list(map(str, argv)), env=env | (extra or {}), stdout=log, stderr=log, check=True, timeout=60)

    def start(argv, extra=None):
        process = subprocess.Popen(list(map(str, argv)), env=env | (extra or {}), stdout=log,
                                   stderr=log, start_new_session=True)
        children.append(process)
        return process

    def request(path, body=None, authenticated=True, bearer=False):
        headers = {'Content-Type': 'application/json', 'Origin': origin}
        if bearer:
            headers['Authorization'] = 'Bearer ' + next(c.value for c in jar if c.name == 'treer_session')
        req = urllib.request.Request(base + path, data=None if body is None else json.dumps(body).encode(), headers=headers)
        try:
            response = (client if authenticated else anonymous).open(req, timeout=30)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            raw = response.read(1024 * 1024 + 1)
            if len(raw) > 1024 * 1024:
                raise RuntimeError('response_limit')
            return response.code, json.loads(raw)

    def api(path, body=None, bearer=False):
        status, value = request(path, body, bearer=bearer)
        if not 200 <= status < 300:
            result['last_api_status'] = status
            result['last_api_error'] = value.get('error', {}).get('code', 'unclassified')
            raise RuntimeError('unexpected_api_status')
        return value

    def wait(fn, seconds=45):
        deadline = time.monotonic() + seconds
        while True:
            try:
                value = fn()
                if value:
                    return value
            except (urllib.error.URLError, TimeoutError, ConnectionError):
                pass
            if time.monotonic() >= deadline:
                raise RuntimeError('readiness_deadline')
            time.sleep(.25)

    def track():
        rows = subprocess.check_output(['/bin/ps', '-ww', '-axo', 'pid=,ppid=,pgid=,lstart=,comm='], text=True)
        owned_pids = set()
        if ws:
            for agent in api(f'/api/workspaces/{ws}/snapshot', bearer=True)['agents']:
                if agent['agent_id'] in (app_id, sender_id) and agent.get('pid'):
                    owned_pids.add(str(agent['pid']))
        records = [row.split(None, 8) for row in rows.splitlines()]
        for fields in records:
            if len(fields) == 9 and (fields[8] in {str(run / 'bin' / n) for n in names} or fields[0] in owned_pids):
                identities.add((fields[0], ' '.join(fields[3:8])))
                owned_pids.add(fields[0])
        # Include observed descendants without reading command lines/environment.
        for _ in range(4):
            for fields in records:
                if len(fields) == 9 and fields[1] in owned_pids:
                    identities.add((fields[0], ' '.join(fields[3:8])))
                    owned_pids.add(fields[0])

    try:
        command([args.pg_bin / 'initdb', '-D', run / 'pg', '--username=treer', '--auth=trust', '--encoding=UTF8', '--locale=en_US.UTF-8'])
        command([args.pg_bin / 'pg_ctl', '-D', run / 'pg', '-l', run / 'pg.log', '-o',
                 f'-h 127.0.0.1 -p {ports["postgres"]} -k {run}', '-w', 'start'])
        pgstarted = True
        command([args.pg_bin / 'createdb', '-h', '127.0.0.1', '-p', ports['postgres'], '-U', 'treer', 'app_tree'])
        admin = secrets.token_urlsafe(32)
        start([run / 'bin/treer-proxy', '--listen', f'127.0.0.1:{ports["proxy"]}', '--public-url', base,
               '--app-public-url', origin, '--ingress-public-url', base, '--artifacts-dir', run / 'artifacts', '--enable-core-messages'],
              {'DATABASE_URL': f'postgres://treer@127.0.0.1:{ports["postgres"]}/app_tree', 'ADMIN_PASSWORD': admin})
        wait(lambda: api('/api/health'))
        api('/api/admin/login', {'password': admin})
        invite = api('/api/admin/invitations', {})['token']
        api('/api/auth/register', {'invite': invite, 'email': 'app-tree@fixture.invalid', 'preferred_name': 'App Tree Owner', 'password': secrets.token_urlsafe(32)})
        org = api('/api/organizations')['organizations'][0]['organization_id']
        ws = api('/api/workspaces', {'organization_id': org, 'name': 'App Tree Local'})['workspace']['workspace_id']
        enrollment = api(f'/api/workspaces/{ws}/bootstrap', {})['enrollment_key']
        result['stage'] = 'native_enrollment'
        start([run / 'bin/treer-agent-server', 'connect', '--proxy', base, '--root', run / 'workspace',
               '--listen', f'127.0.0.1:{ports["controller"]}', '--service-mode', 'foreground', '--name', 'App Tree Native',
               '--non-interactive', '--accept-risk'], {'TREER_ENROLLMENT_KEY': enrollment})

        def health():
            with anonymous.open(f'http://127.0.0.1:{ports["controller"]}/api/health', timeout=4) as response:
                value = json.load(response)
            return value if value.get('proxy_connected') else None

        h = wait(health)
        result['native_reported_source'] = h['controller_build']['git_commit']
        if h['controller_build']['git_commit'] != sha or h['host_build']['git_commit'] != sha:
            raise RuntimeError('native_source_mismatch')
        server_id = h['server_id']
        track()
        result['stage'] = 'app_creation'
        app = api(f'/api/workspaces/{ws}/apps', {'server_id': server_id, 'name': 'hub230', 'command': '/bin/sleep',
                  'args': ['120'], 'cwd': '.', 'port': ports['app'], 'public': False})['app']
        app_id, owner_id = app['app_id'], app['runtime_agent_id']
        app_args = [str(app_source), '--state-dir', str(run / 'app-state'), '--workspace', ws, '--app-agent-id', owner_id]
        command([args.python, '-B'] + app_args + ['seed'])
        api(f'/api/workspaces/{ws}/apps/{app_id}/update', {'command': args.python,
            'args': ['-B'] + app_args + ['serve', '--service-id', app['service_id'], '--proxy-url', base,
                     '--treer', str(run / 'bin/treer'), '--host', '127.0.0.1', '--port', str(ports['app'])]})
        tree_path = f'/api/workspaces/{ws}/apps/{app_id}/ui/api/tree'
        result['stage'] = 'real_recursive_directory'
        wait(lambda: request(tree_path)[0] == 200)
        for path, kind in [('', 'app'), ('team', 'app'), ('team/tools', 'agent')]:
            page = api(tree_path + '?' + urllib.parse.urlencode({'path': path}))
            if page['path'] != (path.split('/') if path else []) or page['nodes'][0]['kind'] != kind:
                raise RuntimeError('recursive_tree_mismatch')
        if request(tree_path, authenticated=False)[0] != 401:
            raise RuntimeError('anonymous_directory_not_denied')
        track()
        result['checks'].append('actual authenticated App API: team App -> tools App -> Agent leaves; anonymous denied')
        result['stage'] = 'actual_cli'
        probe = api(f'/api/workspaces/{ws}/agents', {'server_id': server_id, 'kind': 'command', 'name': 'sender230', 'cwd': '.',
            'args': [args.python, '-B', str(probe_source), '--cli', str(run / 'bin/treer'), '--target', '/hub230/team/tools/agent1', '--owner-id', owner_id,
                     '--output', str(run / 'cli-result.json')]})
        sender_id = probe.get('agent_id') or probe.get('agent', {}).get('agent_id')
        track()
        wait(lambda: (run / 'cli-result.json').exists(), 90)
        cli_result = json.loads((run / 'cli-result.json').read_text())
        result['cli'] = cli_result
        if not cli_result['passed']:
            raise RuntimeError('native_cli_failed')
        result['checks'].extend(cli_result['checks'])
        result['stage'] = 'real_browser'
        write(run / 'browser-fixture.json', {'source_sha': sha, 'proxy_origin': base, 'workspace_id': ws,
            'organization_id': org, 'app_id': app_id, 'owner_id': owner_id,
            'cookie': next(c.name + '=' + c.value for c in jar if c.name == 'treer_session'), 'leaf_path': ['team', 'tools', 'agent1']})
        browser_env = {k: os.environ[k] for k in ('PATH', 'HOME', 'TMPDIR', 'PLAYWRIGHT_BROWSERS_PATH', 'LANG', 'LC_ALL') if k in os.environ}
        browser = subprocess.run([args.node, str(web / 'e2e/app-tree-direct.mjs'), str(run / 'browser-fixture.json'), str(run / 'browser')],
            env=browser_env, cwd=web, stdout=log, stderr=log, timeout=360)
        result['browser'] = json.loads((run / 'browser/result.json').read_text())
        browser_results = result['browser'].get('results', [])
        if browser.returncode or len(browser_results) != 3 or not all(item.get('passed') for item in browser_results):
            raise RuntimeError('browser_failed')
        result['checks'].append('production recursive Tree -> leaf -> human send -> actual App reply in three browsers')
        result['stage'] = 'offline_queue'
        track()
        api(f'/api/workspaces/{ws}/apps/{app_id}/stop', {})
        wait(lambda: api(f'/api/workspaces/{ws}/apps/{app_id}')['app']['status'] == 'stopped')
        body = {'recipients': [owner_id + '/team/tools/agent1'], 'body': 'offline-durable-230', 'idempotency_key': 'offline-230'}
        first = api(f'/api/workspaces/{ws}/messages', body)
        second = api(f'/api/workspaces/{ws}/messages', body)
        if not second['idempotent_replay'] or second['message']['message_id'] != first['message']['message_id']:
            raise RuntimeError('offline_retry_not_same')
        api(f'/api/workspaces/{ws}/apps/{app_id}/start', {})
        mid = first['message']['message_id']
        query = urllib.parse.urlencode({'app_owner_id': owner_id, 'app_endpoint': 'team/tools/agent1', 'limit': 100})
        def offline_reply():
            return next((m for m in api(f'/api/workspaces/{ws}/messages?' + query)['messages']
                         if mid in m['context_ids'] and m['sender'].get('app_endpoint') == 'team/tools/agent1'), None)
        if wait(offline_reply)['body'] != '[app-hub:reply:v1] [team/tools/agent1] offline-durable-230':
            raise RuntimeError('offline_handler_mismatch')
        result['checks'].append('stopped App queues one durable message; retry same ID; resumed App consumes and replies')
        track()
        result.update(passed=True, stage='complete')
    except Exception as error:
        result['failure_type'] = type(error).__name__
    finally:
        errors = []
        if ws and app_id:
            try:
                track()
                api(f'/api/workspaces/{ws}/apps/{app_id}/stop', {})
            except Exception:
                errors.append('app_stop_unproven')
        if ws and sender_id:
            try:
                api(f'/api/workspaces/{ws}/agents/{sender_id}/stop', {})
            except Exception:
                errors.append('sender_stop_unproven')
        if any(cookie.name == 'treer_session' for cookie in jar):
            try:
                api('/api/auth/logout', {})
                if request('/api/auth/me')[0] != 401:
                    errors.append('session_revocation_unproven')
            except Exception:
                errors.append('session_revocation_unproven')
        for process in reversed(children):
            try:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
            except Exception:
                errors.append('owned_process_exit_unproven')
        if pgstarted:
            try:
                command([args.pg_bin / 'pg_ctl', '-D', run / 'pg', '-m', 'fast', '-w', 'stop'])
            except Exception:
                errors.append('postgres_exit_unproven')
        def gone():
            for pid, birth in identities:
                current = subprocess.run(['/bin/ps', '-p', pid, '-o', 'lstart='], capture_output=True, text=True)
                if current.returncode == 0 and current.stdout.strip() == birth:
                    return False
                if current.returncode not in (0, 1) or (current.returncode == 1 and (current.stdout.strip() or current.stderr.strip())):
                    raise RuntimeError('identity_probe_unavailable')
            return True
        identities_gone = False
        try:
            deadline = time.monotonic() + 10
            while not gone() and time.monotonic() < deadline:
                time.sleep(.2)
            identities_gone = bool(identities) and gone()
        except Exception:
            errors.append('identity_exit_unproven')
        absent = {}
        for name, value in ports.items():
            with socket.socket() as stream:
                absent[name] = stream.connect_ex(('127.0.0.1', value)) != 0
        result['cleanup'] = {'roots_reaped': all(p.poll() is not None for p in children), 'captured_identities': len(identities),
                             'identities_gone': identities_gone, 'ports_absent': absent, 'errors': errors}
        result['passed_before_cleanup'] = result['passed']
        result['passed'] = result['passed'] and not errors and identities_gone and all(absent.values())
        log.close()
        write(run / 'result.json', result)
    print(json.dumps({'passed': result['passed'], 'stage': result['stage'], 'receipt': str(run / 'result.json')}))
    return 0 if result['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
