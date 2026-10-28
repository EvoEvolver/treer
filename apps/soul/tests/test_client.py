"""Local-only client workflow: fake Treer, private HOME and real fixture Soul API."""
import hashlib
import http.client
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

from fixture import servers, sealed_environment
def project_key(path):
    import re
    return re.sub(r"[^A-Za-z0-9]", "-", str(Path(path).resolve()))
from soul import bundled_client

APP = Path(__file__).resolve().parents[1]
SID = '01234567-89ab-cdef-0123-456789abcdef'


class ClientTest(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory()
        self.root=Path(self.temp.name)
        self.home=self.root/'home'
        self.home.mkdir()
        self.cwd=self.root/'project'
        self.cwd.mkdir()
        self.tmp=self.root/'tmp'
        self.tmp.mkdir()
        self.log=self.root/'treer.log'
        fake=self.root/'treer'
        fake.write_text('''#!/usr/bin/env python3
import json,os,sys
args=sys.argv[1:]
with open(os.environ['FAKE_LOG'],'a') as log: log.write(json.dumps(args)+'\\n')
if args[:2]==['token','create']: print(os.environ.get('FAKE_TOKEN','owner'))
elif args==['whoami']: print(json.dumps({'agent':{'agent_id':'ag_owner'},'machine':{'server_id':'srv_self'}}))
elif args[:2]==['machine','show']: print(json.dumps({'root':os.environ['HOME']}))
elif args[:2]==['interface','show']: print('null')
elif args[0]=='exec': print(os.environ['FAKE_PROBE'])
elif args[:3]==['agent','admin','create']: print(json.dumps({'agent_id':'ag_new','name':'reborn'}))
else: sys.exit(2)
''')
        fake.chmod(0o755)
        # Fake installed `treer-launch` command (start and `session` subcommand).
        launcher=self.root/'treer-launch'
        launcher.write_text((APP/'tests/fake_session.py').read_text())
        launcher.chmod(0o755)
        self.session_log=self.root/'session.log'
        self.env=sealed_environment({'HOME':str(self.home),'PATH':os.defpath,'TMPDIR':str(self.tmp),'TREER_BIN':str(fake),
                    'FAKE_LOG':str(self.log),'FAKE_SESSION_LOG':str(self.session_log),'FAKE_PROBE':json.dumps(dict(executable=True,version='2.1.0',logged_in=True,launcher=str(launcher),session_transfer=True,free_bytes=1024**3)),
                    'TREER_AGENT_LAUNCHER':str(launcher),'TREER_AGENT_ID':'ag_restore','FAKE_SESSION_SOURCE':str(self.root/'session-source.json')})
        self.server,self.proxy=servers(self.root/'server')
        self.url=f'http://127.0.0.1:{self.server.server_port}'
        self.binary=self.root/'treer-soul'
        self.binary.write_bytes(bundled_client(APP/'client.py',self.url))
        self.binary.chmod(0o755)

    def tearDown(self):
        self.server.shutdown(); self.server.server_close()
        self.proxy.shutdown(); self.proxy.server_close()
        self.temp.cleanup()

    def call(self,*args,env=None,success=True):
        result=subprocess.run([sys.executable,str(self.binary),*args],env=env or self.env,cwd=self.cwd,
                             stdin=subprocess.DEVNULL,capture_output=True,text=True,timeout=15)
        if success: self.assertEqual(result.returncode,0,result.stderr)
        else: self.assertNotEqual(result.returncode,0,result.stdout)
        return result

    def saved_self(self):
        directory=self.cwd/'.treer/agents/ag_owner'
        directory.mkdir(parents=True,exist_ok=True)
        (directory/'agent-ui.json').write_text(json.dumps({'threadId':SID}))

    def source(self,harness,entries):
        (self.root/'session-source.json').write_text(json.dumps(dict(harness=harness,sessionId=SID,files=entries)))

    def claude(self,secret=False):
        self.saved_self()
        project=self.home/'.claude/projects'/project_key(self.cwd)
        (project/'memory').mkdir(parents=True)
        session=project/(SID+'.jsonl')
        data={'sessionId':SID,'cwd':str(self.cwd),'content':'Bearer '+ 'z'*24 if secret else 'learned', 'password':'hunter2'}
        session.write_text(json.dumps(data)+'\n')
        (project/'memory/MEMORY.md').write_text('memory ghp_'+'A'*40+'\n' if secret else 'memory\n')
        (self.cwd/'CLAUDE.md').write_text('project instruction\n')
        self.source('claude',[dict(scope='session',path='session.jsonl',source=str(session)),
                              dict(scope='memory',path='MEMORY.md',source=str(project/'memory/MEMORY.md')),
                              dict(scope='project',path='CLAUDE.md',source=str(self.cwd/'CLAUDE.md'))])
        return session

    def save(self,harness='claude',*extra):
        return json.loads(self.call('save','--harness',harness,'--visibility','workspace',*extra).stdout)

    def test_single_file_save_redacts_copies_and_automatic_self(self):
        source=self.claude(True)
        before=source.read_bytes()
        record=self.save()
        self.assertFalse(record['contains_secrets'])
        self.assertGreaterEqual(record['redaction']['count'],3)
        self.assertEqual(record['created_by'],'ag_owner')
        self.assertEqual(record['source_machine'],'srv_self')
        self.assertEqual(source.read_bytes(),before)
        self.assertEqual(list(self.tmp.iterdir()),[])
        self.assertEqual(sum(f['scope']=='memory' for f in record['manifest']['adapter']['transfer_files']),1)
        self.assertTrue(any(f['scope']=='project' for f in record['manifest']['adapter']['transfer_files']))
        self.assertEqual(self.call('--version').stdout.strip(),'treer-soul 2.0.0')

    def test_explicit_secret_ack_and_default_server_rejection(self):
        self.claude(True)
        denied=self.call('save','--harness','claude','--no-redact',success=False)
        self.assertIn('secrets_detected',denied.stderr)
        self.assertNotIn('hunter2',denied.stderr)
        record=self.save('claude','--no-redact','--allow-secrets')
        self.assertTrue(record['contains_secrets'])
        self.assertEqual(list(self.tmp.iterdir()),[])

    def test_save_and_restore_invoke_installed_treer_launch_session_cli(self):
        self.claude()
        # No override: Soul finds the per-user installed shim, as on a bundle install.
        installed=self.home/'.local/bin/treer-launch'
        installed.parent.mkdir(parents=True)
        installed.write_bytes((self.root/'treer-launch').read_bytes()); installed.chmod(0o755)
        env={k:v for k,v in self.env.items() if k!='TREER_AGENT_LAUNCHER'}
        record=json.loads(self.call('save','--harness','claude','--visibility','workspace',env=env).stdout)
        result=json.loads(self.call('run',record['soul_id'],env=env).stdout)
        calls=[json.loads(line) for line in self.session_log.read_text().splitlines()]
        self.assertEqual([c[:2] for c in calls],[['session','export'],['session','import']])
        export=calls[0]
        self.assertEqual(export[export.index('--config-dir')+1],str((self.home/'.claude').resolve()))
        self.assertEqual(export[export.index('--harness')+1],'claude')
        self.assertEqual(result['argv'][:2],['--harness','claude'])
        self.assertEqual(result['argv'][result['argv'].index('--session-id')+1],calls[1][calls[1].index('--session-id')+1])

    def test_preflight_probe_detects_installed_session_cli(self):
        import client
        installed=self.home/'.local/bin/treer-launch'
        installed.parent.mkdir(parents=True)
        installed.write_bytes((self.root/'treer-launch').read_bytes()); installed.chmod(0o755)
        env={k:v for k,v in self.env.items() if k!='TREER_AGENT_LAUNCHER'}
        probe=json.loads(subprocess.check_output([sys.executable,'-c',client.PREFLIGHT,'claude'],env=env,timeout=30))
        self.assertEqual((probe['launcher'],probe['session_transfer']),(str(installed.resolve()),True))
        old=self.root/'old-treer-launch'
        old.write_text('#!/bin/sh\nexit 1\n'); old.chmod(0o755)
        probe=json.loads(subprocess.check_output([sys.executable,'-c',client.PREFLIGHT,'claude'],env=dict(env,TREER_AGENT_LAUNCHER=str(old)),timeout=30))
        self.assertEqual((probe['launcher'],probe['session_transfer']),(str(old.resolve()),False))

    def test_preflight_falls_back_to_deprecated_treer_agent_alias(self):
        import client
        legacy=self.home/'.local/bin/treer-agent'
        legacy.parent.mkdir(parents=True)
        legacy.write_bytes((self.root/'treer-launch').read_bytes()); legacy.chmod(0o755)
        env={k:v for k,v in self.env.items() if k!='TREER_AGENT_LAUNCHER'}
        probe=json.loads(subprocess.check_output([sys.executable,'-c',client.PREFLIGHT,'claude'],env=env,timeout=30))
        self.assertEqual((probe['launcher'],probe['session_transfer']),(str(legacy.resolve()),True))

    def test_codex_is_not_saved_and_self_requires_authoritative_session(self):
        denied=self.call('save','--harness','codex',success=False)
        self.assertIn('invalid choice',denied.stderr)
        self.assertIn('invalid choice',self.call('capture-codex',success=False).stderr)
        self.claude()
        project=self.home/'.claude/projects'/project_key(self.cwd)
        (project/'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa.jsonl').write_text('{}\n')
        (self.cwd/'.treer/agents/ag_owner/agent-ui.json').unlink()
        self.call('save','--harness','claude',success=False)
        self.call('save','--harness','claude','--session',SID)

    def test_versions_filters_resolution_and_private_refusal(self):
        self.claude()
        first=self.save('claude','--name','my soul')
        second=self.save('claude','--name','my soul','--parent',first['soul_id'])
        self.assertEqual(second['version'],2)
        self.assertEqual(json.loads(self.call('show','my soul@1').stdout)['soul_id'],first['soul_id'])
        listing=json.loads(self.call('list','--name','my soul','--harness','claude','--limit','1').stdout)
        self.assertEqual((listing['total'],listing['next_offset']),(2,1))
        private=json.loads(self.call('save','--harness','claude').stdout)
        self.call('incarnate',private['soul_id'],'--machine','target',success=False)
        commands=[json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertFalse(any(c[:3]==['agent','admin','create'] for c in commands))

    def test_preflight_refusal_force_and_caller_create_folder(self):
        self.claude()
        record=self.save()
        env=dict(self.env,FAKE_PROBE=json.dumps(dict(executable=False,version='1.0.0',logged_in=False,launcher=None,free_bytes=0)))
        self.call('incarnate',record['soul_id'],'--machine','target',env=env,success=False)
        result=self.call('incarnate',record['soul_id'],'--machine','target','--folder','../team','--force',env=env)
        self.assertIn('preflight_warnings',result.stderr)
        commands=[json.loads(line) for line in self.log.read_text().splitlines()]
        create=next(c for c in commands if c[:3]==['agent','admin','create'])
        self.assertEqual(create[create.index('--folder')+1],'../team')
        self.assertNotIn('owner',json.dumps(create))
        self.assertIn('target',create)
        self.assertTrue(any(c[0]=='exec' for c in commands))
        self.call('incarnate',record['soul_id'],'--machine','target')
        commands=[json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertNotIn('--folder',[c for c in commands if c[:3]==['agent','admin','create']][-1])

    def test_first_restore_restart_preserves_new_learning_claude(self):
        self.claude()
        record=self.save()
        original_project=self.home/'.claude/projects'/project_key(self.cwd)
        original=(original_project/(SID+'.jsonl')).read_bytes()
        result=json.loads(self.call('run',record['soul_id']).stdout)
        workspace=Path(result['cwd'])
        self.assertNotEqual(workspace,self.cwd)
        self.assertIn('--cwd',result['argv'])
        self.assertEqual((workspace/'CLAUDE.md').read_text(),'project instruction\n')
        restored=next((Path(result['claude_config'])/'projects').iterdir())
        (restored/'memory/MEMORY.md').write_text('new learning\n')
        restored_session = next(restored.glob('*.jsonl'))
        self.assertNotEqual(restored_session.stem, SID)
        restored_session.write_text('evolved\n')
        log_before=self.log.read_text()
        self.call('run',record['soul_id'])
        self.assertEqual(self.log.read_text(),log_before)
        self.assertEqual((restored/'memory/MEMORY.md').read_text(),'new learning\n')
        self.assertEqual(restored_session.read_text(),'evolved\n')
        self.assertEqual((original_project/(SID+'.jsonl')).read_bytes(),original)

    def test_archived_codex_soul_stays_readable_but_is_not_restored(self):
        soul_id='soul_'+'c'*32
        legacy=self.root/'legacy-codex'
        (legacy/'souls').mkdir(parents=True)
        manifest=dict(schema_version=1,name='Old Codex',environment={},adapter=dict(name='codex',session_id=SID,
            session_file='files/session.jsonl',session_relative_path='2026/10/12/rollout-'+SID+'.jsonl'))
        with tarfile.open(legacy/'souls'/f'{soul_id}.tar','w') as archive:
            for path,data in [('manifest.json',json.dumps(manifest).encode()),('files/session.jsonl',b'{}\n')]:
                member=tarfile.TarInfo(path);member.size=len(data);archive.addfile(member,io.BytesIO(data))
        (legacy/'souls'/f'{soul_id}.json').write_text(json.dumps(dict(soul_id=soul_id,created_at='2025-01-01T00:00:00Z')))
        from soul import SoulStore
        self.server.store.close();self.server.store=SoulStore(legacy);self.server.auth.store=self.server.store
        self.assertEqual(json.loads(self.call('show',soul_id).stdout)['manifest']['adapter']['name'],'codex')
        self.assertIn('Unsupported harness',self.call('incarnate',soul_id,'--machine','target',success=False).stderr)
        self.assertIn('Claude only',self.call('run',soul_id,success=False).stderr)
        self.assertFalse(self.session_log.exists())

    def test_installer_piped_stdin_and_atomic_single_file(self):
        env=dict(self.env,TREER_SOUL_INSTALL_DIR=str(self.root/'bin'))
        connection=http.client.HTTPConnection('127.0.0.1',self.server.server_port)
        connection.request('GET','/install.sh')
        script=connection.getresponse().read(); connection.close()
        result=subprocess.run(['sh'],input=script,capture_output=True,env=env,timeout=15)
        self.assertEqual(result.returncode,0,result.stderr)
        installed=self.root/'bin/treer-soul'
        self.assertTrue(installed.is_file())
        self.assertEqual(subprocess.check_output([sys.executable,str(installed),'--version'],env=env).strip(),b'treer-soul 2.0.0')
        listing=subprocess.run([sys.executable,str(installed),'list'],env=env,stdin=subprocess.DEVNULL,capture_output=True,text=True,timeout=10)
        self.assertEqual(listing.returncode,0,listing.stderr)
        self.assertEqual(json.loads(listing.stdout)['total'],0)
        self.assertEqual(list(self.tmp.iterdir()),[])

    def test_legacy_generic_startup_still_extracts_and_binds(self):
        from test_soul import bundle
        soul_id = 'soul_' + 'a'*32
        legacy = self.root/'legacy'
        (legacy/'souls').mkdir(parents=True)
        (legacy/'souls'/f'{soul_id}.tar').write_bytes(bundle())
        (legacy/'souls'/f'{soul_id}.json').write_text(json.dumps({'soul_id':soul_id,'created_at':'2025-01-01T00:00:00Z'}))
        from soul import SoulStore
        self.server.store.close()
        self.server.store = SoulStore(legacy)
        self.server.auth.store = self.server.store
        record = json.loads(self.call('show', soul_id).stdout)
        self.assertEqual((record['owner'],record['visibility']),('legacy','workspace'))
        # Exact shape of the old service launcher run command, with target's identity.
        result = self.call('run', soul_id, '--', sys.executable, '-c',
                          'import os; print(open(os.environ["SOUL_FILE"]).read().strip())')
        self.assertEqual(result.stdout.strip(), 'remember me')

    def test_native_import_and_extraction_reject_unsafe_inputs(self):
        import client
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream,mode='w') as archive:
            member=tarfile.TarInfo('files/link')
            member.type=tarfile.SYMTYPE
            member.linkname='/etc/passwd'
            archive.addfile(member)
        with self.assertRaises(RuntimeError):
            client.safe_extract(stream.getvalue(), self.root/'unsafe')
        self.claude()
        project=self.home/'.claude/projects'/project_key(self.cwd)
        (project/'memory/MEMORY.md').unlink()
        (project/'memory/MEMORY.md').symlink_to(self.root)
        self.call('save','--harness','claude',success=False)
        self.assertEqual(list(self.tmp.iterdir()),[])

    def test_self_uses_registered_ais_current_id_and_never_default_port(self):
        import client
        from unittest.mock import patch
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        import threading
        class State(BaseHTTPRequestHandler):
            harness='claude'
            def do_GET(self):
                data=json.dumps({'harness':{'id':self.harness,'name':self.harness.title()},'thread':{'id':SID}}).encode()
                self.send_response(200); self.end_headers(); self.wfile.write(data)
            def log_message(self,*_): pass
        server=ThreadingHTTPServer(('127.0.0.1',0),State)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        try:
            with patch.object(client,'treer',return_value=json.dumps({'port':server.server_port})) as cli:
                self.assertEqual(client.self_session('claude'),SID)
                cli.assert_called_once_with('interface','show')
                with self.assertRaises(RuntimeError): client.self_session('codex')
            with patch.object(client,'treer',return_value='null'):
                self.assertIsNone(client.self_session('claude'))
        finally:
            server.shutdown(); server.server_close()

    def test_auth_bearer_never_follows_http_redirect(self):
        import client
        from unittest.mock import patch
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        import threading
        reached=[]
        class Redirect(BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path=='/target':
                    reached.append(self.headers.get('Authorization'))
                    self.send_response(200); self.end_headers()
                else:
                    self.send_response(302); self.send_header('Location','/target'); self.end_headers()
            def log_message(self,*_): pass
        server=ThreadingHTTPServer(('127.0.0.1',0),Redirect)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        try:
            with patch.object(client,'treer',return_value='owner'):
                with self.assertRaises(RuntimeError):
                    client.request(f'http://127.0.0.1:{server.server_port}','GET','redirect')
            self.assertEqual(reached,[])
        finally:
            server.shutdown(); server.server_close()

    def test_redaction_preserves_json_and_removes_complete_or_truncated_keys(self):
        from soul_secrets import redact, scan_secrets
        from client import redact_json
        for raw in (b'-----BEGIN PRIVATE KEY-----\nKEY-BODY\n-----END PRIVATE KEY-----',
                    b'-----BEGIN RSA PRIVATE KEY-----\nKEY-BODY'):
            clean, findings = redact(raw)
            self.assertNotIn(b'KEY-BODY', clean)
            self.assertEqual(findings['private_key'],1)
            self.assertEqual(scan_secrets(clean),{})
        counts={}
        clean=redact_json({'content':'password="hunter2" api_key=abcdef', 'secret':'hidden'},counts)
        self.assertNotIn('hunter2',json.dumps(clean))
        self.assertNotIn('hidden',json.dumps(clean))
        self.assertGreaterEqual(sum(counts.values()),3)

    def test_import_rejects_unrelated_claude_home_selector(self):
        from unittest.mock import patch
        import soul_sessions
        bundle=self.root/'selector-bundle'
        file=bundle/'files/session/session.jsonl'
        file.parent.mkdir(parents=True);file.write_text('{}\n')
        state=self.root/'selector-state';state.mkdir()
        adapter=dict(name='claude',session_id='aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',source_session_id=SID,
                     transfer_files=[dict(scope='session',path='session.jsonl',file='files/session/session.jsonl')])
        local=self.root/'known-claude'
        unrelated=self.root/'someone-else-claude'
        result=dict(sessionId=adapter['session_id'],cwd=str(state/'native'),environment={'CLAUDE_CONFIG_DIR':str(unrelated)})
        with patch.dict(os.environ,{'CLAUDE_CONFIG_DIR':str(local)}), patch.object(soul_sessions,'run_transfer',return_value=result):
            with self.assertRaisesRegex(RuntimeError,'unrelated Claude home'):
                soul_sessions.import_session(bundle,adapter,state/'workspace',state)
            result['environment']['CLAUDE_CONFIG_DIR']=str(local)
            environment,cwd=soul_sessions.import_session(bundle,adapter,state/'workspace',state)
            self.assertEqual(environment['CLAUDE_CONFIG_DIR'],str(local))
            self.assertEqual(Path(cwd),(state/'native').resolve())


if __name__=='__main__':
    environment=sealed_environment()
    os.environ.clear(); os.environ.update(environment)
    unittest.main()
