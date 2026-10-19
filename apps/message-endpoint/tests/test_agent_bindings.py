"""Native hub + synthetic Proxy/CLI; never uses a live bot or workload token."""
import json
from pathlib import Path
import sqlite3
import time
import unittest
from http_fixture import fixture, AGENT, APP_AGENT, ABSENT_AGENT, TOKEN
BINARY=Path(__file__).resolve().parents[3]/'target/debug/treer-message-hub'
class AgentBindings(unittest.TestCase):
    def test_native_agent_shared_create_bind_preview_pause_delete_and_notices(self):
        with fixture(BINARY) as f:
            request=f['request']
            headers={'Authorization':'Bearer '+AGENT,'X-Endpoint-Request':'1','Content-Type':'application/json'}
            def call(command, expected=200):
                s,h,b=request('/api/configuration',headers,'POST',json.dumps(command));self.assertEqual(s,expected,b)
                self.assertIn('x-endpoint-request-id',h);self.assertNotIn(AGENT.encode(),b);return json.loads(b)
            status,_,body=request('/api/settings',headers);self.assertEqual(status,200);self.assertEqual(json.loads(body)['principal_kind'],'agent')
            preview=call({'action':'preview_create','endpoint_id':'ag_new','generation':1,'recipient':'ag_recipient'})['preview']
            create={'action':'create','endpoint_id':'ag_new','generation':1,'recipient':'ag_recipient','expected':preview,'confirm':True,'idempotency_key':'fixture-key'}
            self.assertEqual(call(create),call(create))
            call({**create,'recipient':'ag_other'},409)
            call({'action':'bind_telegram','endpoint_id':'ag_new','revision':1,'target':{'account_id':'bot_fixture','chat_id':577127289,'topic_id':None,'allowed_senders':[577127289]},'confirm':True})
            p=call({'action':'preview_resume','endpoint_id':'ag_new'})
            self.assertEqual(p['preview']['target']['chat_id'],0)
            self.assertEqual(p['preview']['target']['allowed_senders'],[])
            self.assertTrue(p['preview']['confirmation_hash'])
            self.assertFalse(p['activation_enabled'])
            call({'action':'resume','endpoint_id':'ag_new','expected':p['preview'],'confirm':True,'evidence':{'profile_reviewed':True,'isolation':'unverified_accepted','reference':''}},400)
            call({'action':'pause','endpoint_id':'ag_new','revision':2,'confirm':True})
            call({'action':'delete','endpoint_id':'ag_new','revision':3,'confirm':True})
            status,_,body=request('/api/settings',headers);self.assertEqual(status,200)
            self.assertNotIn('ag_new',[e['binding']['endpoint_id'] for e in json.loads(body)['settings']['endpoints']])
            db=sqlite3.connect(f['state']/'hub.sqlite3')
            rows=db.execute("SELECT actor,action FROM hub_owner_audit WHERE endpoint='ag_new'").fetchall()
            self.assertEqual(rows,[('ag_manager',a) for a in ['create_binding','bind_telegram','pause_binding','delete_binding']])
            for _ in range(100):
                if db.execute('SELECT count(*) FROM hub_management_notices WHERE state=1').fetchone()[0]==4:break
                time.sleep(.1)
            self.assertEqual(db.execute('SELECT count(*) FROM hub_management_notices WHERE state=1').fetchone()[0],4)
            notices=db.execute('SELECT recipient,body FROM hub_management_notices').fetchall()
            self.assertTrue(all(to=='ag_recipient' for to,_ in notices))
            self.assertTrue(all('577127289' not in text and 'ag_manager' in text for _,text in notices))
            self.assertTrue(db.execute("SELECT count(*) FROM hub_management_requests WHERE actor='ag_manager' AND status=400").fetchone()[0])
            db.close()
    def test_agent_authentication_csrf_denials_and_owner_token_boundary(self):
        with fixture(BINARY,tokens=True) as f:
            r=f['request']; base={'Authorization':'Bearer '+AGENT,'X-Endpoint-Request':'1','Content-Type':'application/json'}
            preview={'action':'preview_create','endpoint_id':'ag_new','generation':1,'recipient':'ag_recipient'}
            for token,status in [(APP_AGENT,200),(ABSENT_AGENT,403),('forged',401)]:
                self.assertEqual(r('/api/settings',{'Authorization':'Bearer '+token})[0],status)
            for extra in [{'Origin':'https://evil.example'},{'Sec-Fetch-Site':'cross-site'},{'Cookie':'x=1'},{'X-Treer-Identity-Token':'forged'}]:
                self.assertIn(r('/api/configuration',{**base,**extra},'POST',json.dumps(preview))[0],(401,403))
            self.assertEqual(r('/api/configuration',base,'POST',json.dumps({**preview,'actor':'user_owner'}))[0],400)
            self.assertEqual(r('/api/configuration',base,'POST',json.dumps({'action':'export_metadata','kind':'audit','endpoint_id':None}))[0],403)
            trusted={**base,'Origin':'https://hub.example','X-Treer-Identity-Token':AGENT}
            self.assertNotEqual(r('/api/telegram/token',trusted,'POST',json.dumps({'action':'remove','account_id':'bot_fixture','confirm':True}))[0],200)
            db=sqlite3.connect(f['state']/'hub.sqlite3')
            self.assertGreaterEqual(db.execute('SELECT count(*) FROM hub_management_requests WHERE status>=400').fetchone()[0],6)
            self.assertFalse(db.execute("SELECT count(*) FROM endpoints WHERE id='ag_new'").fetchone()[0]);db.close()

    def test_parallel_creation_has_one_receipt_one_notice_and_identical_results(self):
        from concurrent.futures import ThreadPoolExecutor
        with fixture(BINARY) as f:
            headers={'Authorization':'Bearer '+AGENT,'X-Endpoint-Request':'1','Content-Type':'application/json'}
            command={'action':'preview_create','endpoint_id':'ag_new','generation':1,'recipient':'ag_recipient'}
            expected=json.loads(f['request']('/api/configuration',headers,'POST',json.dumps(command))[2])['preview']
            body=json.dumps({**command,'action':'create','expected':expected,'confirm':True,'idempotency_key':'parallel-create'})
            with ThreadPoolExecutor(max_workers=2) as pool:
                results=list(pool.map(lambda _:f['request']('/api/configuration',headers,'POST',body),range(2)))
            self.assertEqual([r[0] for r in results],[200,200],results)
            self.assertEqual(results[0][2],results[1][2])
            with sqlite3.connect(f['state']/'hub.sqlite3') as db:
                self.assertEqual(db.execute('SELECT count(*) FROM hub_create_requests').fetchone()[0],1)
                self.assertEqual(db.execute('SELECT count(*) FROM hub_management_notices').fetchone()[0],1)
    def test_verification_rate_limit_precedes_proxy_calls(self):
        with fixture(BINARY) as f:
            headers={'Authorization':'Bearer '+AGENT}
            statuses=[f['request']('/api/settings',headers)[0] for _ in range(24)]
            self.assertEqual(statuses[:20],[200]*20)
            self.assertEqual(statuses[20:],[429]*4)
            self.assertEqual(f['proxy'].verify_requests,20)
