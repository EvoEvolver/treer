import importlib.util
import os
from unittest.mock import patch
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('smoke', ROOT / 'app-runtime-smoke.py')
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class RuntimeSmokeTests(unittest.TestCase):
    def test_contract_validated_fake_app_roundtrip(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fake = root / 'fake-treer'
            # The fake validates every argv against the shared real clap snapshot.
            fake.write_text('''#!/usr/bin/python3
import json,sys
from pathlib import Path
sys.path.insert(0, APPS_PATH)
from cli_contract import violation
args=sys.argv[1:]
assert violation(args) is None
state=Path(__file__).with_name('state.json')
s=json.loads(state.read_text()) if state.exists() else {}
if args[:2]==['message','send']:
 body=json.load(sys.stdin)['body']
 if s: assert s['body']==body
 else: s={'body':body,'acked':False,'own_id':args[args.index('--to')+1]}
 result={'message':{'message_id':'msg_synthetic'}}
elif args[:2]==['message','receive']:
 result={'deliveries':[] if s['acked'] else [{'delivery_id':'del_synthetic','message':{
  'message_id':'msg_synthetic','body':s['body'],'sender':{'id':s['own_id'],'kind':'agent'}}}]}
elif args[:2]==['message','ack']:
 assert args[2]=='del_synthetic'
 result={'acknowledged_delivery_ids':[] if s['acked'] else ['del_synthetic'],
         'already_acknowledged_delivery_ids':['del_synthetic'] if s['acked'] else []}
 s['acked']=True
else: raise AssertionError('unexpected operation')
state.write_text(json.dumps(s))
print(json.dumps(result))
'''.replace('APPS_PATH', repr(str(ROOT.parents[1]))))
            fake.chmod(0o700)
            for runtime in ['ag_synthetic', 'app_synthetic']:
                fake.with_name('state.json').unlink(missing_ok=True)
                checks = smoke.exercise(str(fake), runtime, 'synthetic123')
                self.assertEqual(len(checks), 7)

    def test_child_timeout_and_output_bound(self):
        with tempfile.TemporaryDirectory() as temporary:
            fake = Path(temporary) / 'fake'
            fake.write_text('#!/usr/bin/python3\nimport time\ntime.sleep(5)\n')
            fake.chmod(0o700)
            with self.assertRaises(smoke.SmokeError):
                smoke.cli(str(fake), [], timeout=0.05)
            fake.write_text('#!/usr/bin/python3\nprint("x"*1100000)\n')
            with self.assertRaises(smoke.SmokeError):
                smoke.cli(str(fake), [])

    def test_failure_response_contains_only_fixed_code_and_step(self):
        for code in [*smoke.FAILURES, 'secret body stderr /private/path app_sensitive']:
            for step in [*smoke.STEPS, 'secret-path-or-response-id']:
                with patch.object(smoke, 'exercise', side_effect=smoke.SmokeError(code, step)):
                    result = smoke.report('/unused', 'app_synthetic', 'synthetic123')
                self.assertEqual(set(result), {'status', 'checks', 'failure', 'step'})
                self.assertEqual(result['status'], 'failed')
                self.assertEqual(result['checks'], [])
                self.assertIn(result['failure'], smoke.FAILURES)
                self.assertIn(result['step'], smoke.STEPS)

    def test_id_validation_accepts_runtime_forms_without_inventing_identity(self):
        with patch.object(smoke, 'cli', side_effect=smoke.SmokeError('cli_unavailable')) as fake:
            for runtime in ['app_12345678', 'ag_12345678', 'other_12345678']:
                result = smoke.report('/unused', runtime, 'synthetic123')
                self.assertEqual(result['failure'], 'cli_unavailable')
                self.assertEqual(result['step'], 'send')
            for runtime in ['', 'human', 'app_short', 'app_12345678\n', '../app_12345678']:
                before = fake.call_count
                result = smoke.report('/unused', runtime, 'synthetic123')
                self.assertEqual(result['failure'], 'invalid_smoke_config')
                self.assertEqual(fake.call_count, before)

    def test_invalid_target_does_not_invoke_cli(self):
        with self.assertRaises(smoke.SmokeError):
            smoke.exercise('/nonexistent', 'human', 'synthetic123')

    def test_missing_executable_has_safe_failure_response(self):
        with tempfile.TemporaryDirectory() as temporary:
            result = smoke.report(str(Path(temporary) / 'missing'), 'app_synthetic', 'synthetic123')
        self.assertEqual(result, {'status': 'failed', 'checks': [],
                                  'failure': 'cli_unavailable', 'step': 'send'})


if __name__ == '__main__':
    unittest.main()
