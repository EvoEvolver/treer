#!/usr/bin/env python3
"""Real executable and loopback doubles; explicitly synthetic identities."""
import json
import sqlite3
from pathlib import Path
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parent))
from http_fixture import TOKEN, fixture

BINARY = Path(sys.argv.pop(1)).resolve()
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
if __name__=='__main__':unittest.main()
