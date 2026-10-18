#!/usr/bin/env python3
"""Real executable and loopback doubles; explicitly synthetic identities."""
import json
import sqlite3
import subprocess
from pathlib import Path
import sys
import time
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parent))
from http_fixture import TOKEN, fixture

BINARY = (Path(sys.argv.pop(1)).resolve() if __name__ == '__main__'
          else Path(__file__).resolve().parents[3] / 'target/debug/treer-message-hub')
class Runtime(unittest.TestCase):
    def test_real_hub_private_storage_identity_and_static_routes(self):
        with fixture(BINARY) as f:
            request=f['request']
            self.assertEqual(request('/api/settings')[0],401)
            self.assertEqual(request('/api/settings',{'X-Treer-Identity-Token':'forged'})[0],401)
            status,headers,body=request('/api/settings',{'X-Treer-Identity-Token':TOKEN})
            self.assertEqual(status,200)
            value=json.loads(body)
            self.assertEqual(value['owner_id'],'user_owner')
            self.assertEqual(value['settings']['endpoints'][0]['binding']['endpoint_id'],'ag_demo')
            self.assertEqual(headers['cache-control'],'no-store')
            self.assertNotIn(TOKEN,body.decode())
            self.assertEqual(request('/api/no-such-route')[0],404)
            self.assertEqual(request('/assets/no-such.js')[0],404)
            self.assertEqual(request('/')[1]['content-type'],'text/markdown; charset=utf-8')
            self.assertEqual(request('/',{'Accept':'text/html'})[1]['content-type'],'text/html; charset=utf-8')
            for path in ['/assets/app.js','/assets/app.css']:
                self.assertEqual(request(path)[0],200)
            self.assertEqual(request('/api/auth/logout',{'X-Treer-Identity-Token':TOKEN,'Origin':'https://evil.example','X-Endpoint-Request':'1'},method='POST')[0],403)
            for p in f['state'].iterdir():
                self.assertEqual(p.stat().st_mode&0o777,0o600)
            self.assertEqual(f['state'].stat().st_mode&0o777,0o700)
    def test_owner_configuration_csrf_strict_json_core_readback_and_atomic_audit(self):
        with fixture(BINARY) as f:
            request=f['request']
            trusted={'X-Treer-Identity-Token':TOKEN,'Origin':'https://hub.example','X-Endpoint-Request':'1','Content-Type':'application/json'}
            def write(command, headers=None, path='/api/configuration'):
                return request(path,trusted if headers is None else headers,method='POST',body=json.dumps(command).encode())[0]
            create={'action':'create','endpoint_id':'ag_new','generation':1,'recipient':'ag_recipient','confirm':True}
            preview_status,_,preview_body=request('/api/configuration',trusted,method='POST',body=json.dumps({'action':'preview_create','endpoint_id':'ag_new','generation':1,'recipient':'ag_recipient'}).encode())
            self.assertEqual(preview_status,200)
            create['expected']=json.loads(preview_body)['preview']
            self.assertNotIn('expected_cwd',create['expected'])
            self.assertEqual(write({**create,'expected':{**create['expected'],'expected_cwd_sha256':'0'*64}}),409)
            self.assertEqual(write(create,{'Content-Type':'application/json'}),403)
            self.assertEqual(write(create,{**trusted,'X-Treer-Identity-Token':'forged'}),401)
            self.assertEqual(write(create,{**trusted,'Origin':'https://evil.example'}),403)
            self.assertEqual(write(create,{**trusted,'Host':'evil.example'}),403)
            self.assertEqual(write(create,{**trusted,'Sec-Fetch-Site':'cross-site'}),403)
            self.assertEqual(write(create,path='/api/configuration?actor=owner'),404)
            for body in [{**create,'actor':'user_other'},{**create,'expected_path':'/invented'}, {**create,'confirm':False},{**create,'endpoint_id':'ag_wrong_workspace'},{**create,'endpoint_id':'ag_unplaced'}]:
                self.assertEqual(write(body),400)
            self.assertEqual(write(create),200)
            # Duplicate creation fails without adding an audit row or changing pins.
            self.assertNotEqual(write(create),200)
            snapshot=json.loads(request('/api/settings',{'X-Treer-Identity-Token':TOKEN})[2])
            endpoint=next(e for e in snapshot['settings']['endpoints'] if e['binding']['endpoint_id']=='ag_new')
            self.assertTrue(endpoint['paused'])
            self.assertEqual(endpoint['binding']['expected_path'],'/team/ag_new')
            bind={'action':'bind_telegram','endpoint_id':'ag_new','revision':1,'confirm':True,'target':{'account_id':'bot_fixture','chat_id':123,'topic_id':None,'allowed_senders':[456]}}
            saved=json.loads(request('/api/configuration',trusted,method='POST',body=json.dumps(bind).encode())[2])
            self.assertEqual(saved,{'status':'saved','revision':2,'paused':True})
            self.assertEqual(write(bind),409)
            self.assertEqual(write({'action':'pause','endpoint_id':'ag_new','revision':2,'confirm':True}),200)
            self.assertEqual(write({'action':'resume','endpoint_id':'ag_new','revision':3,'confirm':True}),400)
            db=sqlite3.connect(f['state']/'hub.sqlite3')
            rows=db.execute('SELECT actor,action FROM hub_owner_audit').fetchall()
            self.assertEqual(rows,[('user_owner','create_binding'),('user_owner','bind_telegram'),('user_owner','pause_binding')])
            self.assertEqual(db.execute("SELECT paused FROM endpoints WHERE id='ag_new'").fetchone()[0],1)
            db.execute("CREATE TRIGGER deny_owner_audit BEFORE INSERT ON hub_owner_audit BEGIN SELECT RAISE(ABORT,'fixture failure'); END")
            db.commit()
            self.assertEqual(write({'action':'pause','endpoint_id':'ag_new','revision':3,'confirm':True}),503)
            self.assertEqual(db.execute("SELECT revision FROM hub_bindings WHERE endpoint='ag_new'").fetchone()[0],3)
            self.assertEqual(db.execute('SELECT COUNT(*) FROM hub_owner_audit').fetchone()[0],3)
            db.execute('DROP TRIGGER deny_owner_audit')
            db.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<9997) INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) SELECT 'user_owner','fixture','ag_new','',1,1 FROM n")
            db.commit()
            self.assertEqual(write({'action':'pause','endpoint_id':'ag_new','revision':3,'confirm':True}),200)
            self.assertEqual(db.execute('SELECT actor,revision,count FROM hub_emergency_pauses').fetchall(),[('user_owner',3,1)])
            snapshot=json.loads(request('/api/settings',{'X-Treer-Identity-Token':TOKEN})[2])
            self.assertTrue(snapshot['settings']['audit_full'])
            self.assertEqual(snapshot['settings']['emergency_pause_bindings'],1)
            db.close()
    def test_owner_recovery_is_metadata_only_revisioned_and_audited(self):
        with fixture(BINARY) as f:
            trusted={'X-Treer-Identity-Token':TOKEN,'Origin':'https://hub.example','X-Endpoint-Request':'1','Content-Type':'application/json'}
            def command(value,headers=None):
                status,_,body=f['request']('/api/configuration',headers or trusted,method='POST',body=json.dumps(value).encode())
                return status,json.loads(body)
            inspect={'action':'inspect_recovery','endpoint_id':'ag_demo','before_audit':None}
            self.assertEqual(command(inspect,{**trusted,'X-Treer-Identity-Token':'forged'})[0],401)
            self.assertEqual(command(inspect,{**trusted,'Origin':'https://evil.example'})[0],403)
            status,result=command(inspect)
            self.assertEqual(status,200)
            self.assertEqual(result['parked'],[])
            db=sqlite3.connect(f['state']/'hub.sqlite3')
            db.execute("INSERT INTO transfers(endpoint,generation,direction,transfer,digest,parts,state,expires) VALUES('ag_demo',1,0,'fixture_transfer','fixture_digest',1,1,9999999999)")
            db.execute("INSERT INTO fragments(endpoint,generation,direction,transfer,part,wire,digest) VALUES('ag_demo',1,0,'fixture_transfer',0,'PRIVATE MESSAGE BODY',x'00')")
            db.execute("INSERT INTO hub_parked(endpoint,generation,transfer,reason,created_at) VALUES('ag_demo',1,'fixture_transfer','transport_rejected',1)")
            db.commit()
            status,result=command(inspect)
            self.assertEqual(status,200)
            self.assertEqual(result['parked'][0]['transfer_id'],'fixture_transfer')
            self.assertNotIn('PRIVATE MESSAGE BODY',json.dumps(result))
            recover={'action':'recover_parked','endpoint_id':'ag_demo','revision':result['revision'],'transfer_id':'fixture_transfer','operation':'discard','confirm':True}
            self.assertEqual(command({**recover,'confirm':False})[0],400)
            self.assertEqual(command({**recover,'actor':'user_other'})[0],400)
            self.assertEqual(command({**recover,'revision':result['revision']+10})[0],409)
            # A live lease forbids destructive recovery.
            db.execute("INSERT INTO telegram_chunks(endpoint,generation,transfer,part,digest,lease_until) VALUES('ag_demo',1,'fixture_transfer',0,x'00',9999999999)")
            db.commit()
            self.assertEqual(command(recover)[0],409)
            db.execute('DELETE FROM telegram_chunks');db.commit()
            status,saved=command(recover)
            self.assertEqual(status,200)
            self.assertEqual(saved['status'],'recovery_status')
            self.assertEqual(saved['parked'],[])
            self.assertTrue(saved['paused'])
            self.assertEqual(saved['revision'],result['revision']+1)
            self.assertEqual(db.execute("SELECT state FROM transfers WHERE transfer='fixture_transfer'").fetchone()[0],4)
            self.assertIsNone(db.execute("SELECT wire FROM fragments WHERE transfer='fixture_transfer'").fetchone()[0])
            self.assertEqual(db.execute("SELECT actor,action FROM hub_owner_audit").fetchall(),[('user_owner','discard_parked')])
            self.assertEqual(command(recover)[0],409)
            status,result=command(inspect)
            self.assertEqual(result['parked'],[])
            self.assertEqual(result['audit'][0]['action'],'discard_parked')
            self.assertEqual(command({**inspect,'before_audit':-1})[0],400)
            db.execute("INSERT INTO transfers(endpoint,generation,direction,transfer,digest,parts,state,expires) VALUES('ag_demo',1,0,'retry_transfer','fixture_digest',1,1,9999999999)")
            db.execute("INSERT INTO fragments(endpoint,generation,direction,transfer,part,wire,digest) VALUES('ag_demo',1,0,'retry_transfer',0,'RETRY PRIVATE BODY',x'00')")
            db.execute("INSERT INTO hub_parked(endpoint,generation,transfer,reason,created_at) VALUES('ag_demo',1,'retry_transfer','transport_rejected',1)")
            db.commit()
            status,retried=command({**recover,'revision':saved['revision'],'transfer_id':'retry_transfer','operation':'retry'})
            self.assertEqual(status,200)
            self.assertTrue(retried['paused'])
            self.assertEqual(db.execute("SELECT wire FROM fragments WHERE transfer='retry_transfer'").fetchone()[0],'RETRY PRIVATE BODY')
            self.assertEqual(db.execute('SELECT COUNT(*) FROM hub_parked').fetchone()[0],0)
            db.executemany("INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) VALUES('user_owner','fixture_page','ag_demo','',1,?)",[(n,) for n in range(25)])
            db.commit()
            status,page=command(inspect)
            self.assertEqual(status,200)
            self.assertEqual(len(page['audit']),25)
            self.assertIsNotNone(page['next_audit_before'])
            status,older=command({**inspect,'before_audit':page['next_audit_before']})
            self.assertEqual(status,200)
            self.assertEqual([a['action'] for a in older['audit']],['retry_parked','discard_parked'])
            self.assertIsNone(older['next_audit_before'])
            db.close()
    def test_running_cli_removal_latches_then_replacement_is_not_executed_and_owner_can_still_pause(self):
        with fixture(BINARY) as f:
            cli=f['state'].parent/'fixture-cli'
            original=cli.read_bytes()
            marker=f['state'].parent/'unapproved-was-executed'
            # Remove first and observe the latch before installing unapproved
            # bytes. Replacing while an interpreter is opening its script would
            # exercise the documented unsupported path-check/exec TOCTOU race.
            cli.rename(cli.with_suffix('.approved'))
            until=time.monotonic()+5
            while True:
                status,_,body=f['request']('/api/settings',{'X-Treer-Identity-Token':TOKEN})
                snapshot=json.loads(body)
                if status==200 and snapshot['settings']['workers']['cli_integrity_failed']:
                    break
                self.assertLess(time.monotonic(),until,'running CLI replacement not detected')
                time.sleep(.2)
            # Calls spawned before the latch can still be opening the old
            # shebang path. Drain only this fixture's direct children first.
            deadline=time.monotonic()+20
            while subprocess.run(['/usr/bin/pgrep','-P',str(f['pid'])],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0:
                self.assertLess(time.monotonic(),deadline,'pre-latch fixture calls did not finish')
                time.sleep(.02)
            cli.write_text('#!/usr/bin/python3\nfrom pathlib import Path\nPath('+repr(str(marker))+').write_text("bad")\n')
            cli.chmod(0o700)
            time.sleep(.3)
            self.assertFalse(marker.exists(),'unapproved CLI was executed after latch')
            cli.write_bytes(original)
            snapshot=json.loads(f['request']('/api/settings',{'X-Treer-Identity-Token':TOKEN})[2])
            self.assertTrue(snapshot['settings']['workers']['cli_integrity_failed'])
            endpoint=snapshot['settings']['endpoints'][0]
            trusted={'X-Treer-Identity-Token':TOKEN,'Origin':'https://hub.example','X-Endpoint-Request':'1','Content-Type':'application/json'}
            status,_,_=f['request']('/api/configuration',trusted,method='POST',body=json.dumps({'action':'pause','endpoint_id':'ag_demo','revision':endpoint['revision'],'confirm':True}).encode())
            self.assertEqual(status,200)
            self.assertFalse(marker.exists())
    def test_owner_resume_is_default_off_and_no_request_can_override_unready_account(self):
        for activation in [False,True]:
            with self.subTest(activation=activation),fixture(BINARY,activation=activation) as f:
                trusted={'X-Treer-Identity-Token':TOKEN,'Origin':'https://hub.example','X-Endpoint-Request':'1','Content-Type':'application/json'}
                def call(value,headers=None):
                    status,_,body=f['request']('/api/configuration',headers or trusted,method='POST',body=json.dumps(value).encode())
                    return status,json.loads(body)
                snapshot=json.loads(f['request']('/api/settings',{'X-Treer-Identity-Token':TOKEN})[2])
                self.assertEqual(snapshot['settings']['activation_enabled'],activation)
                source={'endpoint_id':'ag_new','generation':1,'recipient':'ag_recipient'}
                status,result=call({'action':'preview_create',**source});self.assertEqual(status,200)
                self.assertEqual(call({'action':'create',**source,'expected':result['preview'],'confirm':True})[0],200)
                self.assertEqual(call({'action':'bind_telegram','endpoint_id':'ag_new','revision':1,'target':{'account_id':'bot_fixture','chat_id':123,'topic_id':None,'allowed_senders':[456]},'confirm':True})[0],200)
                status,result=call({'action':'preview_resume','endpoint_id':'ag_new'});self.assertEqual(status,200)
                self.assertEqual(result['activation_enabled'],activation);self.assertFalse(result['account_ready'])
                resume={'action':'resume','endpoint_id':'ag_new','expected':result['preview'],'evidence':{'profile_reviewed':True,'isolation_reviewed':True,'reference':'#159 synthetic fixture'},'confirm':True}
                self.assertEqual(call({**resume,'confirm':False})[0],400)
                self.assertEqual(call({**resume,'activation_enabled':True})[0],400)
                self.assertEqual(call({**resume,'observed':{'path':'/forged'}})[0],400)
                self.assertEqual(call(resume,{**trusted,'Origin':'https://evil.example'})[0],403)
                self.assertEqual(call(resume,{**trusted,'X-Treer-Identity-Token':'forged'})[0],401)
                self.assertEqual(call(resume)[0],409)
                db=sqlite3.connect(f['state']/'hub.sqlite3')
                self.assertEqual(db.execute("SELECT paused FROM endpoints WHERE id='ag_new'").fetchone()[0],1)
                self.assertEqual(db.execute("SELECT COUNT(*) FROM hub_owner_audit WHERE action='resume_binding'").fetchone()[0],0)
                db.close()
    def test_owner_metadata_export_clear_checks_scope_snapshot_and_confirmation(self):
        with fixture(BINARY) as f:
            trusted={'X-Treer-Identity-Token':TOKEN,'Origin':'https://hub.example','X-Endpoint-Request':'1','Content-Type':'application/json'}
            def call(value,headers=None):
                status,_,body=f['request']('/api/configuration',headers or trusted,method='POST',body=json.dumps(value).encode())
                return status,json.loads(body)
            db=sqlite3.connect(f['state']/'hub.sqlite3')
            db.execute("INSERT INTO failures(endpoint,generation,delivery,payload_bytes,reason,created_at) VALUES('ag_demo',1,'delivery_fixture',17,0,1)")
            db.commit()
            export={'action':'export_metadata','kind':'receipts','endpoint_id':'ag_demo'}
            self.assertEqual(call(export,{**trusted,'X-Treer-Identity-Token':'forged'})[0],401)
            status,result=call(export)
            self.assertEqual(status,200)
            archive=result['archive']
            self.assertEqual(archive['version'],1)
            self.assertEqual(len(archive['records']),1)
            self.assertEqual(archive['records'][0]['delivery_id'],'delivery_fixture')
            self.assertNotIn('body',archive['records'][0])
            import hashlib
            self.assertEqual(hashlib.sha256(archive['canonical_jsonl'].encode()).hexdigest(),archive['checksum'])
            self.assertEqual(json.loads(archive['canonical_jsonl'].splitlines()[0])['from'],archive['from'])
            db.execute("INSERT INTO failures(endpoint,generation,delivery,payload_bytes,reason,created_at) VALUES('ag_demo',1,'new_after_export',7,0,2)")
            db.commit()
            clear={'action':'clear_metadata','archive':{k:archive[k] for k in ['kind','endpoint_id','revision','through','checksum']},'archive_saved':True,'confirm':True}
            self.assertEqual(call(clear,{**trusted,'Origin':'https://evil.example'})[0],403)
            self.assertEqual(call({**clear,'archive_saved':False})[0],400)
            self.assertEqual(call({**clear,'confirm':False})[0],400)
            self.assertEqual(call({**clear,'actor':'user_other'})[0],400)
            self.assertEqual(call({**clear,'archive':{**clear['archive'],'checksum':'0'*64}})[0],409)
            status,result=call(clear)
            self.assertEqual((status,result['status']),(200,'metadata_cleared'))
            self.assertEqual(db.execute('SELECT delivery FROM failures').fetchall(),[('new_after_export',)])
            self.assertEqual(db.execute("SELECT paused FROM endpoints WHERE id='ag_demo'").fetchone()[0],1)
            self.assertEqual(call(clear)[0],409)
            status,result=call({'action':'export_metadata','kind':'audit','endpoint_id':None})
            self.assertEqual(status,200)
            archive=result['archive']
            self.assertEqual(archive['records'][0]['action'],'archive_receipts')
            self.assertEqual(archive['records'][0]['actor_id'],'user_owner')
            db.execute("INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) VALUES('user_owner','later','ag_demo','',1,2)")
            db.commit()
            status,result=call({'action':'clear_metadata','archive':{k:archive[k] for k in ['kind','endpoint_id','revision','through','checksum']},'archive_saved':True,'confirm':True})
            self.assertEqual(status,409)
            self.assertEqual([a[0] for a in db.execute('SELECT action FROM hub_owner_audit ORDER BY id')],['archive_receipts','later'])
            db.close()
    def test_metadata_export_read_limit_does_not_block_safety_pause(self):
        with fixture(BINARY) as f:
            trusted={'X-Treer-Identity-Token':TOKEN,'Origin':'https://hub.example','X-Endpoint-Request':'1','Content-Type':'application/json'}
            def call(value):
                status,_,body=f['request']('/api/configuration',trusted,method='POST',body=json.dumps(value).encode())
                return status,json.loads(body)
            export={'action':'export_metadata','kind':'audit','endpoint_id':None}
            snapshot=f['request']('/api/settings',{'X-Treer-Identity-Token':TOKEN})
            self.assertEqual(snapshot[0],200)
            revision=json.loads(snapshot[2])['settings']['endpoints'][0]['revision']
            for _ in range(6):self.assertEqual(call(export)[0],200)
            status,result=call(export)
            self.assertEqual(status,429);self.assertEqual(result['error']['code'],'owner_export_rate_limited')
            self.assertEqual(call({'action':'pause','endpoint_id':'ag_demo','revision':revision,'confirm':True})[0],200)
if __name__=='__main__':unittest.main()
