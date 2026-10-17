#!/usr/bin/env python3
"""Real executable and loopback doubles; explicitly synthetic identities."""
import json
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
if __name__=='__main__':unittest.main()
