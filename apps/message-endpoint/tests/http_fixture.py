#!/usr/bin/env python3
"""Owned loopback-only hub/Proxy/CLI fixture. Never uses installed Treer."""
import contextlib
import hashlib
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time

TOKEN = 'fixture.owner.token'
MEMBER = 'fixture.member.token'
AGENT = 'fixture.agent.token'
APP_AGENT = 'fixture.app.token'
ABSENT_AGENT = 'fixture.absent.token'
THREAD = 'e'*64
PREFIX = '/api/workspaces/ws_test/apps/svc_hub/ui/'
LONG_PATH = '/team/' + 'Long same-named Agent ' * 7 + '<img src=x onerror=alert(1)>'

class Quiet(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def reply(self, status, value):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

class Proxy(Quiet):
    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if size > 16384:
            return self.reply(400, {})
        self.server.verify_requests = getattr(self.server,'verify_requests',0)+1
        body = json.loads(self.rfile.read(size))
        token=body.get('token')
        accepted={AGENT,APP_AGENT,ABSENT_AGENT,MEMBER,TOKEN,TOKEN+'_member',TOKEN+'_agent',TOKEN+'_expired',TOKEN+'_workspace',TOKEN+'_audience'}
        if self.path != '/.treer/apps/identity/verify' or body.get('audience')!='svc_hub' or token not in accepted:
            return self.reply(401, {'active': False})
        if token in (AGENT,APP_AGENT,ABSENT_AGENT):
            return self.reply(200,{'active':True,'claims':{'sub':'ag_app_runtime' if token==APP_AGENT else 'ag_manager','principal_kind':'agent','workspace_id':'ws_test','service_id':'svc_hub','machine_id':'machine_fixture','exp':int(time.time())+300}})
        self.reply(200, {'active': True, 'claims': {'sub': 'user_member' if token==MEMBER else 'user_owner', 'principal_kind': 'agent' if token.endswith('_agent') else 'human', 'workspace_id': 'ws_other' if token.endswith('_workspace') else 'ws_test', 'service_id': 'svc_other' if token.endswith('_audience') else 'svc_hub', 'exp': int(time.time()) + (-1 if token.endswith('_expired') else 300)}})
    def do_GET(self):
        authorization=self.headers.get('Authorization')
        if self.path != '/api/apps/svc_hub/directory' or authorization not in ('Bearer '+AGENT,'Bearer '+APP_AGENT,'Bearer '+ABSENT_AGENT,'Bearer '+TOKEN,'Bearer '+TOKEN+'_member','Bearer '+MEMBER,'Bearer synthetic.app.signature'):
            return self.reply(403, {})
        if authorization in ('Bearer '+AGENT,'Bearer '+APP_AGENT,'Bearer '+ABSENT_AGENT):
            return self.reply(200,{'principals':[] if authorization=='Bearer '+ABSENT_AGENT else [{'id':'ag_app_runtime' if authorization=='Bearer '+APP_AGENT else 'ag_manager','kind':'agent','path':None}]})
        self.reply(200, {'principals': [{'id': 'user_owner', 'kind': 'human', 'role': 'member' if authorization.endswith('_member') else 'owner'},{'id':'user_member','kind':'human','role':'member'}]})

@contextlib.contextmanager
def server(handler):
    instance = ThreadingHTTPServer(('127.0.0.1', 0), handler)
    instance.daemon_threads = True
    thread = threading.Thread(target=instance.serve_forever, daemon=True)
    thread.start()
    try:
        yield instance
    finally:
        instance.shutdown()
        instance.server_close()
        thread.join(timeout=3)

@contextlib.contextmanager
def fixture(binary, editable=False, activation=False, tokens=False, email=False, seed=True):
    with tempfile.TemporaryDirectory(prefix='endpoint-http-fixture-') as temp, server(Proxy) as proxy:
        root = Path(temp).resolve()
        root.chmod(0o700)
        cli = root / 'fixture-cli'
        # Fixed argv, no environment credential access and no network calls.
        cli.write_text('#!/usr/bin/python3\nimport json,sys\nif sys.argv[1:3]==["message","receive"]: print(json.dumps({"deliveries":[],"remaining_unacknowledged":0}))\nelif sys.argv[1:4]==["token","create","svc_hub"]: print(json.dumps({"access_token":"synthetic.app.signature","token_type":"Bearer","audience":"svc_hub","expires_in":300}))\nelif sys.argv[1:4]==["agent","show","--"] and sys.argv[4] in ("ag_demo","ag_new","ag_setup1280","ag_setup390","ag_recipient","ag_wrong_workspace","ag_unplaced"): print(json.dumps({"agent_id":sys.argv[4],"workspace_id":"ws_other" if sys.argv[4]=="ag_wrong_workspace" else "ws_test","server_id":"machine_fixture","kind":"shell" if sys.argv[4]=="ag_recipient" else "command","cwd":"/private/fixture","status":"running","path":None if sys.argv[4]=="ag_unplaced" else "/team/"+sys.argv[4]}))\nelse: sys.exit(1)\n')
        # Native management notices use the hub's own identity, never a recipient's.
        text=cli.read_text().replace('else: sys.exit(1)', 'elif sys.argv[1:3]==["message","send"]:\n import os\n envelope=json.load(sys.stdin)\n target=next(x[5:] for x in sys.argv if x.startswith("--to="))\n log=__file__+".notices"\n with open(log,"a") as f: f.write(json.dumps({"target":target,"body":envelope["body"]})+"\\n")\n os.chmod(log,0o600)\n print(json.dumps({"message":{"message_id":"msg_notice","sender":{"kind":"agent","id":"app_hub"},"body":envelope["body"]}}))\nelse: sys.exit(1)')
        cli.write_text(text)
        cli.chmod(0o700)
        with socket.socket() as reservation:
            reservation.bind(('127.0.0.1', 0))
            port = reservation.getsockname()[1]
        config = root / 'config.json'
        config.write_text(json.dumps({'hub_id':'app_hub','workspace_id':'ws_test','service_id':'svc_hub','proxy_url':f'http://127.0.0.1:{proxy.server_port}/','public_url':f'http://127.0.0.1:{port}/' if editable else 'https://hub.example/','access':'workspace','activation_enabled':activation,'cli':str(cli),'cli_sha256':hashlib.sha256(cli.read_bytes()).hexdigest(),'state_directory':str(root/'state'),'listen':f'127.0.0.1:{port}', 'email':{'outbox_directory':str(root/'outbox')} if email else None, 'telegram_accounts':[{'account_id':'bot_fixture','bot_id':12345,'token_file':str(root/'bot-token.json')}] if tokens else []}))
        config.chmod(0o600)
        environment = {'HOME':str(root),'TMPDIR':str(root),'PATH':'/usr/bin:/bin:/usr/sbin:/sbin','TREER_AGENT_ID':'app_hub','TREER_WORKSPACE_ID':'ws_test'}
        captured_stderr = []
        process = subprocess.Popen([str(binary),'--config',str(config)],env=environment,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        def stop():
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:process.wait(timeout=20)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=3)
        def request(path, headers=None, method='GET', body=None):
            connection = http.client.HTTPConnection('127.0.0.1',port,timeout=3)
            try:
                connection.request(method,path,body=body,headers=headers or {})
                response=connection.getresponse()
                return response.status,dict(response.getheaders()),response.read()
            finally:
                connection.close()
        try:
            for _ in range(100):
                if process.poll() is not None:
                    raise AssertionError('fixture hub exited before health')
                try:
                    if request('/health')[0]==200:
                        break
                except (OSError,http.client.HTTPException):
                    time.sleep(.05)
            else:
                raise AssertionError('fixture hub did not listen')
            if seed:
                db=sqlite3.connect(root/'state/hub.sqlite3')
                binding={'endpoint_id':'ag_demo','generation':1,'recipient':'ag_recipient','expected_path':'/team/ag_demo' if email else LONG_PATH,'expected_cwd':'/private/fixture','expected_server_id':'machine_fixture'}
                db.execute('INSERT INTO endpoints(id,generation,recipient,paused) VALUES(?,?,?,1)',('ag_demo',1,'ag_recipient'))
                db.execute('INSERT INTO hub_bindings(endpoint,definition) VALUES(?,?)',('ag_demo',json.dumps(binding)))
                if email:
                    db.execute("UPDATE endpoints SET paused=0 WHERE id='ag_demo'")
                    db.execute("INSERT INTO email_targets(endpoint,generation,address,member) VALUES('ag_demo',1,'person@example.test','user_member')")
                    db.execute("INSERT INTO email_threads VALUES(?,'ag_demo',1,'ag_recipient','person@example.test','user_member')",(THREAD,))
                    db.execute("INSERT INTO email_messages VALUES(?,'msg_browser','<script>window.PWNED=1</script> & confidential text','fixture_digest',?)",(THREAD,int(time.time())))
                db.commit();db.close()
            def forward_handler(embedded):
                class Forward(Quiet):
                    def do_GET(self):
                        if embedded and self.path=='/console':
                            page=f'<!doctype html><title>Fixture Console</title><iframe title="Endpoint App" src="{PREFIX}{"#email-thread="+THREAD if email else ""}" style="width:100%;height:1200px;border:0"></iframe>'.encode()
                            self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(page)));self.end_headers();self.wfile.write(page);return
                        if embedded and not self.path.startswith(PREFIX):
                            return self.reply(404,{})
                        path='/'+self.path[len(PREFIX):] if embedded else self.path
                        # Model only the documented forwarding contract: strip
                        # client identity, inject a test verified owner identity.
                        h={k:v for k,v in self.headers.items() if not k.lower().startswith('x-treer-') and k.lower() not in ('host','cookie')}
                        h['X-Treer-Identity-Token']=MEMBER if email else TOKEN
                        status,headers,body=request(path,h)
                        self.send_response(status)
                        for k,v in headers.items():
                            if k.lower() in ('connection','server','date','transfer-encoding','content-length'):continue
                            if embedded and k.lower()=='content-security-policy':
                                v=';'.join(p for p in v.split(';') if not p.strip().startswith('frame-ancestors'))
                            self.send_header(k,v)
                        self.send_header('Content-Length',str(len(body)))
                        self.end_headers();self.wfile.write(body)
                return Forward
            with server(forward_handler(False)) as wildcard, server(forward_handler(True)) as embedded:
                yield {'url':f'http://127.0.0.1:{port}/','wildcard':f'http://127.0.0.1:{wildcard.server_port}/','embedded':f'http://127.0.0.1:{embedded.server_port}/console','prefix':PREFIX,'request':request,'pid':process.pid,'stop':stop,'state':root/'state','root':root,'stderr':captured_stderr,'proxy':proxy,'auditdb':root/'state/management-audit/hub.sqlite3'}
        finally:
            stop()
            if process.stderr:
                captured_stderr.append(process.stderr.read())
                process.stderr.close()

if __name__=='__main__':
    signal.signal(signal.SIGTERM,lambda *_:sys.exit(0))
    with fixture(Path(sys.argv[1]).resolve(),editable='--editable' in sys.argv[2:],tokens='--token-slots' in sys.argv[2:],email='--email' in sys.argv[2:]) as f:
        if '--seed-email-recovery' in sys.argv[2:]:
            # New synthetic state only; workers disabled, never a live mail store.
            db=sqlite3.connect(f['state']/'hub.sqlite3')
            b={'endpoint_id':'ag_recovery','generation':1,'recipient':'ag_recipient','expected_path':'/team/ag_recovery','expected_cwd':'/private/fixture','expected_server_id':'machine_fixture'}
            db.execute("INSERT INTO endpoints(id,generation,recipient,paused) VALUES('ag_recovery',1,'ag_recipient',1)")
            db.execute("INSERT INTO hub_bindings(endpoint,definition) VALUES('ag_recovery',?)",(json.dumps(b),))
            db.execute("INSERT INTO email_targets(endpoint,generation,address,member) VALUES('ag_recovery',1,'person@example.test','user_member')")
            db.execute("INSERT INTO email_threads VALUES(?,'ag_recovery',1,'ag_recipient','person@example.test','user_member')",('c'*64,))
            db.execute("INSERT INTO email_messages VALUES(?,'msg_old','synthetic retained mail','digest_old',61)",('c'*64,))
            db.execute("INSERT INTO email_intents(id,thread,endpoint,generation) VALUES(?,?,'ag_recovery',1)",('a'*64,'c'*64))
            db.execute("INSERT INTO email_dispatch VALUES(?,'ag_recovery',1,?,'uncertain')",('a'*64,json.dumps({'text':'Private notification\n\nReference: 0123456789abcdef','idempotency_key':'synthetic-key'}).encode()))
            db.commit();db.close()
        if '--seed-recovery' in sys.argv[2:]:
            db=sqlite3.connect(f['state']/'hub.sqlite3')
            db.execute("INSERT INTO hub_activity(endpoint,generation,activated_at,stale_updates) VALUES('ag_demo',1,?,1)",(int(time.time()),))
            db.execute("INSERT INTO transfers(endpoint,generation,direction,transfer,digest,parts,state,expires) VALUES('ag_demo',1,0,'fixture_transfer','fixture_digest',1,1,9999999999)")
            db.execute("INSERT INTO fragments(endpoint,generation,direction,transfer,part,wire,digest) VALUES('ag_demo',1,0,'fixture_transfer',0,'PRIVATE RECOVERY BODY',x'00')")
            db.execute("INSERT INTO hub_parked(endpoint,generation,transfer,reason,created_at) VALUES('ag_demo',1,'fixture_transfer','transport_rejected',1)")
            db.execute("INSERT INTO failures(endpoint,generation,delivery,payload_bytes,reason,created_at) VALUES('ag_demo',1,'fixture_receipt',17,0,?)",(int(time.time()),))
            db.commit();db.close()
        print(json.dumps({**{k:f[k] for k in ['url','wildcard','embedded','prefix']},'thread':THREAD}),flush=True)
        while True:time.sleep(1)
