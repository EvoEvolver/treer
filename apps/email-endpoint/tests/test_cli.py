from test_endpoint import e  # Clears inherited runtime identity/proxy variables first.
import json
import os
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


class CLI(unittest.TestCase):
    def test_path_resolution_send_status_contacts_and_redirect_refusal(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            fake=root/'treer'
            fake.write_text('''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
p=Path(os.environ['TEST_CLI_LOG'])
with p.open('a') as out: out.write(json.dumps(sys.argv[1:])+'\\n')
if sys.argv[1:3]==['agent','ls']:
 print(json.dumps({'agents':[{'agent_id':'app_endpoint','kind':'app','name':'mail'}]}))
elif sys.argv[1:3]==['app','show']:
 print(json.dumps({'app':{'runtime_agent_id':'other' if os.environ.get('TEST_CLI_WRONG') else 'app_endpoint','service_id':'svc_fake'}}))
else: raise SystemExit(2)
''')
            fake.chmod(0o700)
            calls=[]
            class Proxy(BaseHTTPRequestHandler):
                def log_message(self,*_): pass
                def do_GET(self): self.respond()
                def do_POST(self): self.respond()
                def respond(self):
                    body=self.rfile.read(int(self.headers.get('Content-Length',0)))
                    calls.append((self.command,self.path,self.headers,body))
                    response=json.dumps({'ok':True}).encode()
                    self.send_response(302 if self.path.endswith('/redirect') else 200)
                    self.send_header('Location','http://leak.invalid/')
                    self.send_header('Content-Type','application/json')
                    self.send_header('Content-Length',str(len(response)))
                    self.end_headers();self.wfile.write(response)
            server=ThreadingHTTPServer(('127.0.0.1',0),Proxy)
            threading.Thread(target=server.serve_forever,daemon=True).start()
            env={**os.environ,'EMAIL_ENDPOINT_TREER_BIN':str(fake),'TEST_CLI_LOG':str(root/'calls'),
                 'http_proxy':f'http://127.0.0.1:{server.server_address[1]}','no_proxy':''}
            cli=str(e.ROOT/'email-endpoint')
            def run(*args,extra=None):
                return subprocess.run([cli,*args],input='private body',text=True,capture_output=True,
                                      env=env | (extra or {}),timeout=10)
            try:
                result=run('send','--endpoint','../team/mail','--to','contact','--subject','Review','--idempotency-key','fixed')
                self.assertEqual(result.returncode,0,result.stderr)
                self.assertTrue(json.loads(result.stdout)['ok'])
                method,path,headers,body=calls[-1]
                self.assertEqual(path,'http://svc_fake.treer/v1/notify')
                self.assertEqual(json.loads(body)['body'],'private body')
                self.assertEqual(headers['Idempotency-Key'],'fixed')
                argv=[json.loads(line) for line in (root/'calls').read_text().splitlines()]
                self.assertEqual(argv[:2],[['agent','ls','../team'],['app','show','app_endpoint']])
                self.assertNotIn('private body',(root/'calls').read_text())
                self.assertEqual(run('status','--endpoint','/team/mail','--notification','nt_one').returncode,0)
                self.assertEqual(run('contacts','--endpoint','mail').returncode,0)
                before=len(calls)
                result=run('contacts','--endpoint','/team/mail',extra={'TEST_CLI_WRONG':'1'})
                self.assertNotEqual(result.returncode,0)
                self.assertEqual(len(calls),before)
                result=run('status','--url','http://svc_fake.treer/','--notification','redirect')
                self.assertNotEqual(result.returncode,0)
                self.assertEqual(len(calls),before+1)
            finally:
                server.shutdown();server.server_close()

if __name__=='__main__': unittest.main()
