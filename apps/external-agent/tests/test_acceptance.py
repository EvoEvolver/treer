"""Client mechanics only: these mocks are never real identity/Policy acceptance."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('acceptance', Path(__file__).with_name('acceptance.py'))
a = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(a)


def manifest():
    return dict(public_url='https://external.example/', owner_url='https://owner.example/',
                workspace='ws_a', foreign_workspace='ws_b', app='app_a', agent='ag_a',
                allowed='ag_allowed', denied='ag_denied', foreign='ag_foreign', generation='generation_a',
                decision='fixture decision reference', policy_evidence_sha256='a'*64,
                token_file='/unavailable/token', owner_headers_file='/unavailable/headers')


class ClientTests(unittest.TestCase):
    def test_plan_never_opens_credentials_or_network(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d)/'manifest.json'
            p.write_text(json.dumps(manifest()))
            result = subprocess.run([sys.executable, str(Path(a.__file__)), str(p)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            value = json.loads(result.stdout)
            self.assertEqual(value['status'], 'NOT RUN')
            self.assertFalse(value['secrets_read'])

    def test_scope_hash_binds_targets_and_separate_origins(self):
        m = manifest()
        digest = a.validate(m)
        changed = dict(m, denied='ag_another')
        self.assertNotEqual(digest, a.validate(changed))
        for changes in [dict(owner_url=m['public_url']), dict(foreign_workspace='ws_a'), dict(public_url='https://user:secret@external.example'), dict(allowed='../api')]:
            with self.assertRaises(ValueError):
                a.validate(dict(m, **changes))

    def test_actual_loopback_stack_is_allowed_but_cleartext_network_is_not(self):
        for origin in ['http://127.0.0.1:18080/', 'http://[::1]:18080/']:
            self.assertTrue(a.validate(dict(manifest(), public_url=origin)))
        for origin in ['http://localhost:18080/', 'http://192.0.2.1:18080/',
                       'http://127.0.0.1:0/', 'http://127.0.0.1:18080/?token=x']:
            with self.assertRaises(ValueError):
                a.validate(dict(manifest(), public_url=origin))

    def test_embedded_owner_prefix_is_exact_and_scope_bound(self):
        m = manifest()
        prefix = '/api/workspaces/ws_a/apps/app_a/ui'
        for suffix in ['', '/']:
            self.assertTrue(a.validate(dict(m, owner_url='http://127.0.0.1:18081'+prefix+suffix)))
        for path in [prefix.replace('ws_a', 'ws_other'), prefix.replace('app_a', 'app_other'),
                     prefix+'/owner/state', prefix+'/../ui', prefix.replace('ws_a', '%77s_a'),
                     '//api/workspaces/ws_a/apps/app_a/ui', prefix+'?next=elsewhere']:
            with self.assertRaises(ValueError):
                a.validate(dict(m, owner_url='https://owner.example'+path))
        with self.assertRaises(ValueError):
            a.validate(dict(m, public_url='https://external.example'+prefix))
        with self.assertRaises(ValueError):
            a.validate(dict(m, owner_url='https://external.example'+prefix))
        changed = dict(m, owner_url='https://owner.example'+prefix)
        self.assertNotEqual(a.validate(m), a.validate(changed))

    def test_transport_preserves_fixed_owner_prefix_without_redirect(self):
        prefix = '/api/workspaces/ws_a/apps/app_a/ui'
        for suffix in ['', '/']:
            with patch.object(a.http.client, 'HTTPConnection') as client:
                response = client.return_value.getresponse.return_value
                response.status = 200
                response.read.return_value = b'{}'
                a.request('http://127.0.0.1:18081'+prefix+suffix, 'POST', '/owner/revoke',
                          {'Cookie': 'fixture-only', 'Origin': 'http://127.0.0.1:18081'}, {'id': 'fixed'})
                call = client.return_value.request.call_args
                self.assertEqual(call.args[1], prefix+'/owner/revoke')
                self.assertEqual(call.args[3]['Origin'], 'http://127.0.0.1:18081')
                self.assertEqual(client.return_value.request.call_count, 1)
                client.return_value.close.assert_called_once()

    def test_nine_requests_audit_before_revoke_and_no_retry(self):
        m = dict(manifest(), owner_url='https://owner.example/api/workspaces/ws_a/apps/app_a/ui/')
        token = 'ext1_'+'1'*32+'.'+'2'*64
        identity = dict(workspace=m['workspace'], app=m['app'], agent=m['agent'])
        rows = [dict(request_id=str(i), token='1'*32, operation=op, target=m[target], outcome=outcome)
                for i, (op, target, outcome) in enumerate([
                    ('status', 'allowed', 'completed'), ('transcript', 'denied', 'policy_denied'),
                    ('stop', 'denied', 'policy_denied'), ('transcript', 'foreign', 'unavailable')])]
        replies = [(200, dict(identity=identity, generation=m['generation'], enabled=True)),
                   (200, dict(identity=identity, requests=[])), (200, dict(agent_id=m['allowed'])),
                   (403, dict(error=dict(code='policy_denied'))), (403, dict(error=dict(code='policy_denied'))),
                   (503, dict(error=dict(code='unavailable'))), (200, dict(identity=identity, requests=rows)),
                   (200, dict(revoked=True)), (401, dict(error=dict(code='authentication_required')))]
        for i in range(4):
            status, value = replies[i+2]
            replies[i+2] = (status, dict(request_id=str(i), result=value))
        secrets = [token, json.dumps({'Cookie': 'fixture-only'})]
        with patch.object(a, 'private_text', side_effect=secrets), patch.object(a, 'request', side_effect=copy.deepcopy(replies)) as send:
            value = a.execute(m)
            self.assertEqual(send.call_count, 9)
            for call in send.call_args_list:
                if call.args[2].startswith('/owner/'):
                    self.assertEqual(call.args[0], m['owner_url'])
                    self.assertEqual(call.args[3]['Origin'], 'https://owner.example')
                    self.assertNotIn('Authorization', call.args[3])
                else:
                    self.assertEqual(call.args[0], m['public_url'])
                    self.assertNotIn('Cookie', call.args[3])
            self.assertEqual(send.call_args_list[7].args[2], '/owner/revoke')
            self.assertEqual(value['foreign_nonexecution'], 'EXTERNAL EVIDENCE REQUIRED')
        replies[3] = (503, dict(request_id='1', result=dict(error=dict(code='unavailable'))))
        with patch.object(a, 'private_text', side_effect=secrets), patch.object(a, 'request', side_effect=replies) as send:
            with self.assertRaises(ValueError):
                a.execute(m)
            self.assertEqual(send.call_count, 4)
            self.assertFalse(any(c.args[2] == '/owner/revoke' for c in send.call_args_list))


if __name__ == '__main__':
    unittest.main()
