"""New setup seam only: private bundle, bounded discovery and optional native relay."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'scripts/prepare-email-relay.py'
spec = importlib.util.spec_from_file_location('setup', SCRIPT)
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class Setup(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.root.chmod(0o700)
        self.cli = self.root / 'treer'
        self.cli.write_text('''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
root=Path(__file__).parent
args=sys.argv[1:]
assert 'TREER_OPERATOR_TOKEN' not in os.environ
assert 'TREER_EXTERNAL_TOKEN' not in os.environ
if args[:4]==['--format','json','agent','show']:
 assert os.environ['TREER_IDENTITY_MODE']=='agent'
 target=args[-1]
 obj=json.loads((root/'metadata.json').read_text())[target]
 print(json.dumps(obj))
else:
 state=root/'received.json'
 principal={'id':'ag_relay','kind':'agent','name':'Relay'}
 if args[:2]==['message','send']:
  envelope=json.load(sys.stdin)
  body=json.loads(envelope['body'])
  old=json.loads(state.read_text()) if state.exists() else {}
  old[body['protocol']]=True
  state.write_text(json.dumps(old))
  result={'message':{'message_id':'msg_test','sender':principal,'body':envelope['body']}}
 elif args[:2]==['message','receive']:
  old=json.loads(state.read_text()) if state.exists() else {}
  result={'remaining_unacknowledged':0,'deliveries':[] if old.get('acked') else [{'delivery_id':'del_test','recipient':principal,'message':{'message_id':'msg_in','sender':{'kind':'agent','id':'ag_recipient','name':'Recipient'},'body':'closed setup test','context_ids':[]}}]}
 elif args[:2]==['message','ack']:
  old=json.loads(state.read_text()); assert old.get('treer.endpoint/v1')
  old['acked']=True; state.write_text(json.dumps(old))
  result={'acknowledged_delivery_ids':['del_test'],'already_acknowledged_delivery_ids':[]}
 else: raise AssertionError(args)
 print(json.dumps(result))
''')
        self.cli.chmod(0o700)
        self.relay = self.root / 'relay'
        self.relay.write_text('#!/bin/sh\nexit 0\n')
        self.relay.chmod(0o700)
        self.objects = {x: {'agent_id': x, 'workspace_id': 'ws_test',
                           'server_id': 'srv_test', 'kind': 'app' if x == 'app_hub' else 'command',
                           'status': 'exited', 'path': '/test/'+x, 'cwd': 'relay-work'}
                        for x in ('app_hub', 'ag_relay', 'ag_recipient')}
        self.save()
        self.env = patch.dict(os.environ, {'TREER_AGENT_ID':'ag_setup',
            'TREER_WORKSPACE_ID':'ws_test', 'TREER_SERVER_ID':'srv_test', 'TREER_WORKLOAD_CREDENTIAL':'synthetic-no-real-secret',
            'TREER_OPERATOR_TOKEN':'must-not-inherit','TREER_EXTERNAL_TOKEN':'must-not-inherit'})
        self.env.start()
        self.addCleanup(self.env.stop)

    def save(self):
        (self.root/'metadata.json').write_text(json.dumps(self.objects))

    def args(self):
        return setup.parser().parse_args(['--hub','app_hub','--endpoint','ag_relay',
            '--recipient','ag_recipient','--workspace','ws_test','--cli',str(self.cli),
            '--cli-sha256',hashlib.sha256(self.cli.read_bytes()).hexdigest(),
            '--relay',str(self.relay),'--relay-sha256',hashlib.sha256(self.relay.read_bytes()).hexdigest(),
            '--output',str(self.root/'bundle')])

    def test_bundle_private_and_no_activation_or_credential_copy(self):
        result=setup.prepare(self.args())
        self.assertEqual(result['mutations'],0)
        self.assertFalse(result['channel_activation'])
        for path in (self.root/'bundle').iterdir():
            self.assertEqual(stat.S_IMODE(path.stat().st_mode),0o600)
            self.assertNotIn('synthetic-no-real-secret',path.read_text())
        launch=json.loads((self.root/'bundle/launch.json').read_text())
        self.assertEqual(launch['required_agent_id'],'ag_relay')
        self.assertEqual(launch['argv'][0],str(self.relay))
        self.assertFalse((self.root/'received.json').exists())
        with self.assertRaises(FileExistsError): setup.prepare(self.args())

    def test_foreign_workspace_and_running_relay_refuse(self):
        for field,value in [('workspace_id','ws_foreign'),('status','running'),('server_id','srv_foreign')]:
            original=self.objects['ag_relay'][field]
            self.objects['ag_relay'][field]=value; self.save()
            with self.assertRaises(ValueError): setup.prepare(self.args())
            self.assertFalse((self.root/'bundle').exists())
            self.objects['ag_relay'][field]=original

    def test_bad_binary_pin_refuses_before_discovery(self):
        args=self.args(); args.cli_sha256='0'*64
        with self.assertRaisesRegex(ValueError,'binary_pin'): setup.prepare(args)
        self.assertFalse((self.root/'bundle').exists())

    def test_metadata_timeout_and_output_bound(self):
        for code,expected in [('import time; time.sleep(3)','metadata_timeout'),
                              ('print("x"*262145)','metadata_too_large')]:
            self.cli.write_text('#!/usr/bin/env python3\n'+code+'\n')
            started=time.monotonic()
            with self.assertRaisesRegex(ValueError,expected):
                setup.metadata(self.cli,'ag_relay',dict(os.environ),timeout=0.2)
            self.assertLess(time.monotonic()-started,2)

    @unittest.skipUnless(os.environ.get('TREER_SETUP_TEST_RELAY'), 'native relay path not provided')
    def test_generated_config_drives_native_relay_start_forward_ack(self):
        self.relay=Path(os.environ['TREER_SETUP_TEST_RELAY']).resolve()
        setup.prepare(self.args())
        with subprocess.Popen([str(self.relay),'--config',str(self.root/'bundle/relay.json')],
                env={'PATH':os.environ['PATH'],'HOME':str(self.root),'TREER_AGENT_ID':'ag_relay'},
                stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL) as child:
            try:
                deadline=time.monotonic()+12
                while time.monotonic()<deadline:
                    state=self.root/'received.json'
                    try: receipt=json.loads(state.read_text())
                    except (FileNotFoundError,json.JSONDecodeError): receipt={}
                    if receipt.get('acked'):
                        self.assertTrue(receipt['treer.endpoint.status/v1'])
                        self.assertTrue(receipt['treer.endpoint/v1'])
                        return
                    self.assertIsNone(child.poll())
                    time.sleep(0.05)
                self.fail('native relay did not forward and ack')
            finally:
                child.kill(); child.wait()


if __name__ == '__main__': unittest.main()
