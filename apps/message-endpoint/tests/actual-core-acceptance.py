#!/usr/bin/env python3
"""Run Hub email/outbox on a NEW real Core loopback stack; never discover an installed enrollment.

Supply pinned native binaries and separately built production Hub/relay binaries.
Only fresh task-owned PostgreSQL/process/App identities are created and stopped.
The final JSON is safe to share; the private fixture directory is not.
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
    parser.add_argument('--proxy', type=Path, required=True)
    parser.add_argument('--hub-bin', type=Path, required=True)
    parser.add_argument('--relay-bin', type=Path, required=True)
    parser.add_argument('--core-sha', required=True)
    parser.add_argument('--python', default='/usr/bin/python3')
    args = parser.parse_args()
    os.umask(0o077)
    source, native = (p.resolve(strict=True) for p in (args.source, args.native_dir))
    sha = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
    names = ('treer', 'treer-agent-server', 'treer-agent-host', 'treer-proxy')
    inputs = {name: (args.proxy if name == 'treer-proxy' else native / name).resolve(strict=True) for name in names}
    pins = {name: digest(inputs[name]) for name in names}
    app_source = args.hub_bin.resolve(strict=True)
    probe_source = source / 'apps/message-endpoint/tests/actual-core-probe.py'
    run = Path(tempfile.mkdtemp(prefix='hub204-', dir='/private/tmp'))
    for leaf in ('bin', 'home', 'config', 'state', 'runtime', 'workspace'):
        (run / leaf).mkdir(mode=0o700)
    for name in names:
        shutil.copyfile(inputs[name], run / 'bin' / name)
        (run / 'bin' / name).chmod(0o700)
        if digest(run / 'bin' / name) != pins[name]:
            raise RuntimeError('native_copy_mismatch')
    ports = dict(zip(('postgres', 'proxy', 'controller', 'app'), (port() for _ in range(4))))
    ports['policy'] = port()
    if len(set(ports.values())) != len(ports):
        raise RuntimeError('port_collision')
    for value in ports.values():
        with socket.socket() as stream:
            if stream.connect_ex(('127.0.0.1', value)) == 0:
                raise RuntimeError('requested_port_occupied')
    base = 'http://127.0.0.1:' + str(ports['proxy'])
    origin = f'http://127.0.0.1:{ports["app"]}'
    env = {'PATH': str(run / 'bin') + ':/usr/bin:/bin', 'HOME': str(run / 'home'),
           'XDG_CONFIG_HOME': str(run / 'config'), 'XDG_STATE_HOME': str(run / 'state'),
           'XDG_RUNTIME_DIR': str(run / 'runtime'), 'TREER_STATE_DIR': str(run / 'state'),
           'TREER_RUNTIME_DIR': str(run / 'runtime'), 'TREER_NETWORK_MODE': 'plain',
           'LANG': 'en_US.UTF-8', 'LC_ALL': 'en_US.UTF-8', 'RUST_LOG': 'warn'}
    result = {'source_sha': sha, 'scope': 'actual Core identity, Policy and Rust Hub/relay email outbox/member reply; no mail transport',
              'passed': False, 'stage': 'bootstrap', 'policy_source': args.core_sha, 'checks': [], 'native_sha256': pins,
              'app_source_sha256': digest(app_source), 'probe_sha256': digest(probe_source),
              'driver_sha256': digest(Path(__file__)),
              'settings_source_sha256': digest(source/'apps/message-endpoint/src/settings_http.rs'),
              'relay_sha256': digest(args.relay_bin), 'real_channel_traffic': 'NOT_RUN', 'installed_mutation': False}
    write(run / 'intent.json', {'source_sha': sha, 'ports': ports, 'private_resources_only': True})
    log = (run / 'private.log').open('xb')
    children, identities, pgstarted = [], set(), False
    ws, app_id, sender_id = None, None, None
    owned_agents = []
    owned_apps = []
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

    def request(path, body=None, authenticated=True, bearer=False, method=None, extra=None):
        headers = {'Content-Type': 'application/json', 'Origin': origin}
        if bearer:
            headers['Authorization'] = 'Bearer ' + next(c.value for c in jar if c.name == 'treer_session')
        headers.update(extra or {})
        req = urllib.request.Request(base + path, data=None if body is None else json.dumps(body).encode(), headers=headers, method=method)
        try:
            response = (client if authenticated else anonymous).open(req, timeout=30)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            raw = response.read(1024 * 1024 + 1)
            if len(raw) > 1024 * 1024:
                raise RuntimeError('response_limit')
            return response.code, json.loads(raw)

    def api(path, body=None, bearer=False, method=None, extra=None):
        status, value = request(path, body, bearer=bearer, method=method, extra=extra)
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
                if agent['agent_id'] in owned_agents + owned_apps and agent.get('pid'):
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
        pgpassword=secrets.token_urlsafe(32)
        (run/'pg-password').write_text(pgpassword)
        command([args.pg_bin / 'initdb', '-D', run / 'pg', '--username=treer', '--auth=scram-sha-256', '--pwfile', run/'pg-password', '--encoding=UTF8', '--locale=en_US.UTF-8'])
        command([args.pg_bin / 'pg_ctl', '-D', run / 'pg', '-l', run / 'pg.log', '-o',
                 f'-h 127.0.0.1 -p {ports["postgres"]} -k {run}', '-w', 'start'])
        pgstarted = True
        command([args.pg_bin / 'createdb', '-h', '127.0.0.1', '-p', ports['postgres'], '-U', 'treer', 'app_tree'], {'PGPASSWORD':pgpassword})
        admin = secrets.token_urlsafe(32)
        start([run / 'bin/treer-proxy', '--listen', f'127.0.0.1:{ports["proxy"]}', '--public-url', base,
               '--app-public-url', origin, '--ingress-public-url', base, '--artifacts-dir', run / 'artifacts', '--enable-core-messages'],
              {'DATABASE_URL': f'postgres://treer:{pgpassword}@127.0.0.1:{ports["postgres"]}/app_tree', 'ADMIN_PASSWORD': admin})
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
        if h['controller_build']['git_commit'] != args.core_sha or h['host_build']['git_commit'] != args.core_sha:
            raise RuntimeError('native_source_mismatch')
        server_id = h['server_id']
        track()
        result['stage'] = 'hub_and_relay_creation'
        app = api(f'/api/workspaces/{ws}/apps', {'server_id': server_id, 'name': 'hub204', 'command': '/bin/sleep',
                  'args': ['300'], 'cwd': '.', 'port': ports['app'], 'public': False})['app']
        app_id, owner_id = app['app_id'], app['runtime_agent_id']
        owned_apps.append(app_id)
        probe = api(f'/api/workspaces/{ws}/agents', {'server_id': server_id, 'kind': 'command', 'name': 'recipient204', 'cwd': '.',
            'args': [args.python, '-B', str(probe_source), str(run), 'recipient']})
        sender_id = probe.get('agent_id') or probe.get('agent', {}).get('agent_id')
        owned_agents.append(sender_id)
        relay = api(f'/api/workspaces/{ws}/agents', {'server_id': server_id, 'kind': 'command', 'name': 'relay204', 'cwd': '.',
            'args': [args.python, '-B', str(probe_source), str(run), 'relay', str(args.relay_bin.resolve())]})
        relay_id = relay.get('agent_id') or relay.get('agent', {}).get('agent_id')
        owned_agents.append(relay_id)
        write(run / 'relay-config.json', dict(endpoint_id=relay_id, hub_id=owner_id, generation=1,
            recipients=[sender_id], cli=str(run/'bin/treer'), cli_sha256=pins['treer'],
            state_directory=str(run/'relay-state'), release_sha256=digest(args.relay_bin)))
        result['stage']='actual_policy_provider'
        assets={'treer-policy.py':'policy.py','treer-policy-client.py':'treer-policy','treer-policy-agent.md':'AGENT.md','treer-policy-index.html':'web/index.html','treer-policy-app.css':'web/app.css','treer-policy-app.js':'web/app.js'}
        policy_pins={}
        for dest,src in assets.items():
            raw=subprocess.check_output(['git','-C',str(source),'show',args.core_sha+':apps/policy/'+src])
            (run/'workspace'/dest).write_bytes(raw)
            policy_pins[dest]=hashlib.sha256(raw).hexdigest()
        pa=api(f'/api/workspaces/{ws}/apps',dict(server_id=server_id,name='policy204',command='/usr/bin/python3',
            args=['treer-policy.py','--port',str(ports['policy']),'--initialize-admin-token-file'],cwd='.',port=ports['policy'],public=False))['app']
        owned_apps.append(pa['app_id'])
        tokenfile=run/'workspace/.treer-policy-admin-token'
        wait(lambda:tokenfile.exists())
        api(f'/api/workspaces/{ws}/policy-provider',dict(app_id=pa['app_id'],failure_mode='fail_closed',max_stale_seconds=300),method='PUT')
        policy_token=tokenfile.read_text().strip()
        doc=dict(schema_version=1,defaults={'message.send':'deny'},groups={},rules=[dict(id='exact-private-message-actors',priority=50,effect='allow',
            subjects=[dict(kind='agent',id=x) for x in [owner_id,sender_id,relay_id]],actions=['message.send'],resources=[{}])])
        req=urllib.request.Request(f"http://127.0.0.1:{ports['policy']}/v1/policy/publish",data=json.dumps(dict(expected_revision=1,mode='enforce',document=doc)).encode(),
            headers={'Content-Type':'application/json','X-Policy-Admin-Token':policy_token})
        with anonymous.open(req,timeout=5) as response: published=json.load(response)
        if published['bundle']['mode']!='enforce':raise RuntimeError('provider_mode')
        result['policy']=dict(assets=policy_pins,document_sha256=hashlib.sha256(json.dumps(doc,sort_keys=True).encode()).hexdigest(),mode='enforce',revision=published['bundle']['revision'])
        public = f'http://127.0.0.1:{ports["app"]}/'
        write(run/'hub-config.json', dict(hub_id=owner_id,workspace_id=ws,service_id=app['service_id'],
            proxy_url=base,public_url=public,virtual_hostname=app['hostname'],listen=f'127.0.0.1:{ports["app"]}',access='workspace',activation_enabled=True,
            cli=str(run/'bin/treer'),cli_sha256=pins['treer'],state_directory=str(run/'hub-state'),
            email=dict(mode='outbox',outbox_directory=str(run/'outbox'))))
        api(f'/api/workspaces/{ws}/apps/{app_id}/update', {'command': str(app_source),
            'args': ['--config',str(run/'hub-config.json')]})
        settings = f'/api/workspaces/{ws}/apps/{app_id}/ui/'
        result['stage'] = 'actual_owner_configuration'
        def settings_ready():
            status,value=request(settings+'api/settings')
            result['settings_http_status']=status
            result['settings_http_error']=value.get('error',{}).get('code')
            if status in (400,401,403):raise RuntimeError('settings_' + str(result['settings_http_error']))
            return status==200
        wait(settings_ready)
        result['checks'].append('actual managed Rust Hub + Proxy-signed owner settings')
        if request(settings+'api/settings',authenticated=False)[0] not in (401,403):
            raise RuntimeError('anonymous_settings_not_denied')
        def configure(body):
            return api(settings+'api/configuration',body,extra={'X-Endpoint-Request':'1','Origin':public.rstrip('/')})
        preview=configure(dict(action='preview_create',endpoint_id=relay_id,recipient=sender_id,generation=1))['preview']
        configure(dict(action='create',endpoint_id=relay_id,recipient=sender_id,generation=1,expected=preview,confirm=True))
        me=api('/api/auth/me')
        human=me.get('user',me).get('user_id') or me.get('user',me).get('id')
        if not human: raise RuntimeError('human_identity_missing')
        configure(dict(action='bind_email',endpoint_id=relay_id,revision=1,contact=dict(address='app-tree@fixture.invalid',member_id=human),confirm=True))
        write(run/'relay-release.json',dict(start=True))
        def ready():
            v=configure(dict(action='preview_resume',endpoint_id=relay_id))
            return v if v.get('account_ready') and v.get('activation_enabled') and (api(settings+'api/settings')['settings']['endpoints'][0].get('diagnostic') or {}).get('state')=='running' else None
        p=wait(ready,60)
        configure(dict(action='resume',endpoint_id=relay_id,expected=p['preview'],confirm=True,
            evidence=dict(profile_reviewed=True,isolation='unverified_accepted',reference='NEW private real Core fixture; no OS isolation or production claim',held_action='discard',parked_count_ack=0)))
        result['checks'].append('real CLI metadata + current directory binding + relay capability + owner resume')
        track()
        write(run/'send.json',dict(endpoint=relay_id))
        result['stage']='actual_core_outbox'
        def outbox_ready():
            files=list((run/'outbox').glob('*.json')) if (run/'outbox').exists() else []
            return files if files else None
        files=wait(outbox_ready,90)
        if len(files)!=1:raise RuntimeError('duplicate_outbox')
        notice=json.loads(files[0].read_text())
        if 'private core fixture body' in json.dumps(notice):raise RuntimeError('notification_body_leaked')
        thread=notice['text'].split('#email-thread=')[1].split()[0]
        thread_path=settings+'api/email/thread/'+thread
        v=api(thread_path)
        if v['thread']['messages'][0]['body']!='private core fixture body':raise RuntimeError('member_body_mismatch')
        result['checks'].append('actual Core send/receive/ACK -> Rust relay/Hub -> one private outbox; real member reads')
        for _ in range(2):
            api(thread_path,dict(key='actual-same-key',body='actual member reply'),extra={'X-Endpoint-Request':'1','Origin':public.rstrip('/')})
        wait(lambda:(run/'probe-result.json').exists(),90)
        probe=json.loads((run/'probe-result.json').read_text())
        if not probe.get('passed'):raise RuntimeError('actual_reply_failed')
        result['probe']=probe
        result['checks'].append('actual member HTTP reply duplicate key -> relay -> Core authenticated recipient/context, ACK')
        track()
        result.update(passed=True,stage='complete')
    except Exception as error:
        if ws and app_id:
            try:
                result['failed_app_status']=api(f'/api/workspaces/{ws}/apps/{app_id}')['app']['status']
                o=api(f'/api/workspaces/{ws}/agents/{app_id}/output?lines=20',bearer=True)
                text=o.get('text','')
                result['app_fixed_errors']=[code for code in ['hub_configuration_unavailable','hub_storage_unavailable','hub_runtime_unavailable','owner_auth_configuration_invalid'] if code in text]
            except Exception: result['failure_diagnostic']='unavailable'
        result['failure_type'] = type(error).__name__
        result['failure_code'] = str(error) if isinstance(error,RuntimeError) else 'private_detail_redacted'
    finally:
        errors = []
        if ws:
            try: track()
            except Exception: errors.append('final_identity_capture_unproven')
        for aid in owned_apps:
            try:
                api(f'/api/workspaces/{ws}/apps/{aid}/stop', {})
            except Exception:
                errors.append('owned_app_stop_unproven')
        for aid in owned_agents:
            try:
                api(f'/api/workspaces/{ws}/agents/{aid}/stop', {})
            except Exception:
                errors.append('owned_agent_stop_unproven')
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
