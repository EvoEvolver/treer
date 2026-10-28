"""Mechanics only, not actual Core or Policy acceptance."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import output_containment as c
from test_acceptance import manifest


class OutputClientTests(unittest.TestCase):
    def inputs(self):
        return dict(manifest(), expected_output_sha256=hashlib.sha256(b'fixture output\n').hexdigest())

    def replies(self):
        m = self.inputs()
        identity = {k: m[k] for k in ('workspace', 'app', 'agent')}
        state = dict(identity=identity, generation=m['generation'], enabled=True)
        rows = [dict(request_id='req_'+str(i), token='1'*32, operation='output',
                     target=m[t], outcome=o)
                for i, (t, o) in enumerate([('allowed', 'completed'), ('denied', 'policy_denied')])]
        return [
            (200, state), (200, dict(identity=identity, requests=[])),
            (200, dict(request_id='req_0', result=dict(agent_id=m['allowed'], revision=3,
                        text='fixture output\n', truncated=False))),
            (403, dict(request_id='req_1', result=dict(error=dict(code='policy_denied')))),
            (200, dict(identity=identity, requests=rows)), (200, dict(enabled=False)),
            (200, dict(state, enabled=False)), (503, dict(error=dict(code='disabled'))),
            (200, dict(identity=identity, requests=rows)),
        ]

    def run_client(self, replies):
        with patch.object(c.base, 'private_text', side_effect=[
                'ext1_'+'1'*32+'.'+'2'*64, '{"Cookie":"synthetic"}']), \
                patch.object(c.base, 'request', side_effect=replies) as request:
            result = c.execute(self.inputs())
            return result, request.call_args_list

    def test_exact_nine_requests_and_separated_authority(self):
        result, calls = self.run_client(self.replies())
        self.assertEqual(result['requests'], 9)
        self.assertEqual(len(calls), 9)
        self.assertEqual(calls[5].args[1:3], ('POST', '/owner/activation'))
        self.assertEqual(calls[5].args[4], dict(enabled=False, generation='generation_a',
                                             agent='ag_a', policy_reviewed=False))
        for call in calls:
            headers = call.args[3]
            if call.args[0] == self.inputs()['public_url']:
                self.assertEqual(set(headers), {'Authorization'})
            else:
                self.assertNotIn('Authorization', headers)

    def test_each_failure_stops_without_retry(self):
        for index in range(9):
            replies = self.replies()
            replies[index] = (500, {})
            with patch.object(c.base, 'private_text', side_effect=[
                    'ext1_'+'1'*32+'.'+'2'*64, '{"Cookie":"synthetic"}']), \
                    patch.object(c.base, 'request', side_effect=replies) as request:
                with self.assertRaises(ValueError):
                    c.execute(self.inputs())
                self.assertEqual(request.call_count, index+1)

    def test_wrong_target_pin_or_truncation_refused_before_disable(self):
        for change in [dict(agent_id='other'), dict(text='wrong'),
                       dict(truncated=True), dict(revision=True)]:
            replies = self.replies()
            replies[2][1]['result'].update(change)
            with self.assertRaises(ValueError):
                self.run_client(replies)

    def test_disabled_request_must_not_add_audit(self):
        replies = self.replies()
        replies[-1][1]['requests'] = replies[-1][1]['requests'] + [
            dict(request_id='unexpected', token='1'*32, operation='output')]
        with self.assertRaises(ValueError):
            self.run_client(replies)

    def test_plan_does_not_read_secret_or_connect(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d)/'manifest.json'
            p.write_text(json.dumps(self.inputs()))
            r = subprocess.run([sys.executable, str(Path(c.__file__)), str(p)],
                               capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertFalse(json.loads(r.stdout)['secrets_read'])

    def test_output_pin_is_scope_bound(self):
        self.assertNotEqual(c.validate(self.inputs()),
                            c.validate(dict(self.inputs(), expected_output_sha256='b'*64)))
        with self.assertRaises(ValueError):
            c.validate(dict(self.inputs(), expected_output_sha256='invalid'))


if __name__ == '__main__':
    unittest.main()
