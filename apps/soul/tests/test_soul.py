from __future__ import annotations

import concurrent.futures
import hashlib
import http.client
import io
import json
import os
import sqlite3
import tarfile
import tempfile
import unittest
from pathlib import Path
from urllib.parse import urlencode

from fixture import servers, TOKENS, PRINCIPALS, sealed_environment
from soul import SoulStore, SoulError, scan_secrets


def bundle(content=b'remember me\n', name='Test soul', manifest=None):
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode='w') as archive:
        for path, data in [('manifest.json', json.dumps(manifest or {'schema_version': 1, 'name': name, 'environment': {'SOUL_FILE': 'files/state.txt'}}).encode()), ('files/state.txt', content)]:
            info = tarfile.TarInfo(path)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    return stream.getvalue()


class SoulServerTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name) / 'data'
        self.server, self.proxy = servers(self.root)

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.proxy.shutdown()
        self.proxy.server_close()
        self.temporary.cleanup()

    def request(self, method, path, body=None, token='owner', headers=None):
        connection = http.client.HTTPConnection('127.0.0.1', self.server.server_port, timeout=10)
        hdrs = {} if token is None else {'X-Treer-Identity-Token': token}
        if body:
            hdrs['Content-Type'] = 'application/x-tar'
        hdrs.update(headers or {})
        connection.request(method, path, body=body, headers=hdrs)
        response = connection.getresponse()
        raw = response.read()
        connection.close()
        return response.status, json.loads(raw) if response.getheader('Content-Type', '').startswith('application/json') else raw

    def upload(self, query='', content=b'remember me\n', token='owner', name='Test soul'):
        status, record = self.request('POST', '/v1/souls' + query, bundle(content, name), token)
        self.assertEqual(status, 201, record)
        return record

    def test_private_workspace_readers_and_immutable_ownership(self):
        private = self.upload()
        self.assertEqual(private['owner'], 'ag_owner')
        self.assertEqual(private['visibility'], 'private')
        for suffix in ('', '/archive'):
            self.assertEqual(self.request('GET', '/v1/souls/' + private['soul_id'] + suffix, token='other')[0], 404)
        self.assertEqual(self.request('DELETE', '/v1/souls/' + private['soul_id'], token='other')[0], 404)
        public = self.upload('?visibility=workspace')
        self.assertEqual(self.request('GET', '/v1/souls/' + public['soul_id'], token='other')[0], 200)
        self.assertEqual(self.request('DELETE', '/v1/souls/' + public['soul_id'], token='other')[0], 403)
        readable = self.upload('?reader=ag_other')
        self.assertEqual(self.request('GET', '/v1/souls/' + readable['soul_id'], token='other')[0], 200)
        self.assertEqual(self.request('POST', '/v1/souls?reader=made_up', bundle())[0], 400)
        self.assertEqual(self.request('POST', '/v1/souls?owner=ag_other', bundle())[0], 400)
        self.assertEqual(self.request('GET', '/v1/souls', token='other')[1]['total'], 2)

    def test_authentication_priority_and_browser_csrf(self):
        for token in (None, '', 'forged', 'expired', 'wrong-audience'):
            for method, path, body in [('GET', '/v1/souls', None), ('POST', '/v1/souls', bundle())]:
                self.assertEqual(self.request(method, path, body, token, {'Authorization': 'Bearer owner', 'Cookie': 'soul_session=valid'})[0], 401 if token is not None else 200 if method == 'GET' else 201)
        self.assertEqual(self.request('GET', '/v1/me', token=None)[0], 401)
        # Header trumps bearer; false self-reported ID cannot affect verified principal.
        status, result = self.request('GET', '/v1/me', token='other', headers={'X-Treer-Agent-Id':'ag_owner'})
        self.assertEqual((status, result['principal']['id']), (200, 'ag_other'))
        for headers in ({}, {'X-Soul-Request': '1', 'Origin':'null'}, {'X-Soul-Request':'1', 'Sec-Fetch-Site':'cross-site'}):
            self.assertEqual(self.request('POST', '/v1/souls', bundle(), 'human', headers)[0], 403)
        headers = {'X-Soul-Request':'1', 'Origin':self.server.public_url, 'Sec-Fetch-Site':'same-origin'}
        status, record = self.request('POST', '/v1/souls', bundle(), 'human', headers)
        self.assertEqual(status, 201, record)
        self.assertEqual(record['owner'], 'u_human')
        # Directory membership checked every time.
        PRINCIPALS.remove(TOKENS['other'])
        try:
            self.assertEqual(self.request('GET', '/v1/me', token='other')[0], 403)
        finally:
            PRINCIPALS.append(TOKENS['other'])

    def test_scan_every_file_reject_or_acknowledge_without_leaks(self):
        examples = [b'enr_v1_7773_enrollment.' + b'a'*64, b'wlc_' + b'a'*64, b'enrollment_token="' + b'a'*32 + b'"', b'srv_abc.'+b'A'*32,
                    b'sk-' + b'a'*24, b'ghp_' + b'A'*40, b'github_pat_'+b'Z'*30,
                    b'Bearer '+b'z'*28, b'eyJabcdef.abcdef.signature', b'-----BEGIN RSA PRIVATE KEY-----',
                    b'AKIA'+b'A'*16, b'password=hunter2', b'secret=abc123', b'api_key=abcdef']
        for example in examples:
            status, error = self.request('POST', '/v1/souls', bundle(example))
            self.assertEqual(status, 422, (example, error))
            self.assertTrue(error['error']['findings'])
            self.assertNotIn(example.decode(), json.dumps(error))
        record = self.upload('?allow_secrets=true', b'Bearer '+b'z'*28)
        self.assertTrue(record['contains_secrets'])
        self.assertEqual(record['secret_findings'], {'bearer':1})
        self.assertEqual(self.request('POST', '/v1/souls?allow_secrets=1', bundle())[0], 400)
        # Manifest name itself scanned, even if no environment binding.
        self.assertEqual(self.request('POST','/v1/souls',bundle(name='ghp_'+ 'Q'*40))[0], 422)

    def test_versions_latest_filters_pagination_and_parent(self):
        first = self.upload('?harness=claude&label=work')
        second = self.upload('?harness=claude&parent_soul_id='+first['soul_id'])
        self.assertEqual((first['version'],second['version']), (1,2))
        self.assertEqual(second['parent_soul_id'], first['soul_id'])
        self.upload('?harness=codex', name='Different')
        status, latest = self.request('GET', '/v1/souls/by-name/Test%20soul?version=latest&owner=ag_owner')
        self.assertEqual(status,200)
        self.assertEqual(latest['soul_id'], second['soul_id'])
        self.assertEqual(self.request('GET','/v1/souls/by-name/Test%20soul?version=1')[1]['soul_id'],first['soul_id'])
        status, listing = self.request('GET','/v1/souls?'+urlencode({'name':'Test soul','harness':'claude','owner':'ag_owner','limit':1}))
        self.assertEqual((status,listing['total'],listing['next_offset']), (200,2,1))
        self.assertEqual(self.request('GET', '/v1/souls?offset=1&limit=1')[1]['next_offset'], 2)
        self.assertEqual(self.request('GET', '/v1/souls/by-name/Test%20soul',token='other')[0],404)
        foreign = self.upload('?visibility=workspace',token='other')
        self.assertEqual(self.request('POST','/v1/souls?parent_soul_id='+foreign['soul_id'], bundle())[0],403)
        for query in ('limit=101','offset=-1','limit=latest','unknown=x','limit=2&limit=3'):
            self.assertEqual(self.request('GET','/v1/souls?'+query)[0],400)

    def test_quota_deletion_dedup_and_compatible_archive(self):
        self.server.store.max_count = 2
        first, second = self.upload(), self.upload()
        self.assertEqual(len(list(self.server.store.blobs.iterdir())),2)
        status, raw = self.request('GET','/v1/souls/'+first['soul_id']+'/archive')
        self.assertEqual(status,200)
        self.assertEqual(hashlib.sha256(raw).hexdigest(),first['archive_sha256'])
        with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
            self.assertEqual(archive.extractfile('files/state.txt').read(), b'remember me\n')
        self.assertEqual(self.request('POST','/v1/souls',bundle())[0],409)
        self.assertEqual(self.request('GET','/v1/me')[1]['usage']['count'],2)
        self.assertEqual(self.request('DELETE','/v1/souls/'+first['soul_id'])[0],200)
        self.assertEqual(len(list(self.server.store.blobs.iterdir())),2)
        self.assertEqual(self.request('DELETE','/v1/souls/'+first['soul_id'])[0],200)
        self.assertEqual(self.request('GET','/v1/souls/'+first['soul_id'])[0],404)
        third = self.upload()
        self.assertEqual(third['version'],3)
        self.request('DELETE','/v1/souls/'+second['soul_id'])
        self.request('DELETE','/v1/souls/'+third['soul_id'])
        self.assertEqual(list(self.server.store.blobs.iterdir()),[])
        self.server.store.max_bytes = 1
        self.assertEqual(self.request('POST','/v1/souls',bundle())[0],409)
        self.assertEqual(list(self.server.store.blobs.iterdir()),[])

    def test_concurrent_version_and_quota(self):
        self.server.store.max_count = 3
        with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
            results = list(pool.map(lambda _: self.request('POST','/v1/souls',bundle()), range(6)))
        self.assertEqual(sorted(r[1]['version'] for r in results if r[0]==201), [1,2,3])
        self.assertEqual(sum(r[0]==409 for r in results),3)

    def test_incarnation_never_uses_server_authority_and_public_docs(self):
        record = self.upload()
        self.assertEqual(self.request('POST','/v1/souls/'+record['soul_id']+'/incarnations',b'{}')[0],410)
        for accept, expected in [('text/html',b'<!doctype html>'), ('text/markdown',b'# Treer Soul')]:
            status, body = self.request('GET','/',token=None,headers={'Accept':accept})
            self.assertEqual(status,200)
            self.assertTrue(body.startswith(expected))
        self.assertEqual(self.request('GET','/unknown')[0],404)
        self.assertEqual(self.request('GET','/health',token=None)[0],200)

    def test_legacy_migration_full_backup_restart_and_rollback(self):
        other_root = Path(self.temporary.name)/'old'
        (other_root/'souls').mkdir(parents=True)
        soul_id = 'soul_' + 'a'*32
        archive = bundle(b'old state')
        (other_root/'souls'/f'{soul_id}.tar').write_bytes(archive)
        old = {'soul_id':soul_id,'created_at':'2025-01-01T00:00:00Z','manifest':{'name':'Test soul'}}
        (other_root/'souls'/f'{soul_id}.json').write_text(json.dumps(old))
        store = SoulStore(other_root)
        backup = store.backup
        self.assertEqual((backup/'souls'/f'{soul_id}.tar').read_bytes(), archive)
        record = store.get(soul_id,{'id':'ag_other'})
        self.assertEqual((record['owner'],record['visibility']),('legacy','workspace'))
        with tarfile.open(fileobj=io.BytesIO(store.archive(soul_id,{'id':'ag_other'}))) as tar:
            self.assertEqual(tar.extractfile('files/state.txt').read(),b'old state')
        with self.assertRaises(SoulError):
            store.delete(soul_id,{'id':'ag_other'})
        store.close()
        store = SoulStore(other_root)
        self.assertIsNone(store.backup)
        self.assertEqual(store.get(soul_id,{'id':'ag_other'})['soul_id'],soul_id)
        store.close()
        # Backup is a usable v1 directory, no v2 database or altered archive.
        self.assertFalse((backup/'index.sqlite3').exists())

    def test_oauth_fallback_cookie_and_invalid_header_precedence(self):
        from urllib.parse import urlsplit, parse_qs, quote
        connection = http.client.HTTPConnection('127.0.0.1', self.server.server_port)
        connection.request('GET','/api/auth/login')
        response = connection.getresponse()
        self.assertEqual(response.status,303)
        redirect=response.getheader('Location'); response.read()
        state=parse_qs(urlsplit(redirect).query)['state'][0]
        connection.request('GET','/api/auth/callback?code=test&state='+quote(state))
        response=connection.getresponse()
        self.assertEqual(response.status,303)
        cookie=response.getheader('Set-Cookie').split(';',1)[0]
        response.read(); connection.close()
        status, me=self.request('GET','/v1/me',token=None,headers={'Cookie':cookie})
        self.assertEqual((status,me['principal']['id']),(200,'u_human'))
        self.assertEqual(self.request('GET','/v1/me',token='forged',headers={'Cookie':cookie})[0],401)
        self.assertEqual(self.request('POST','/v1/souls',bundle(),token=None,headers={'Cookie':cookie})[0],403)
        headers={'Cookie':cookie,'X-Soul-Request':'1','Origin':self.server.public_url}
        self.assertEqual(self.request('POST','/v1/souls',bundle(),token=None,headers=headers)[0],201)
        self.assertEqual(self.request('GET','/api/auth/callback?code=test&state='+quote(state),token=None)[0],401)

    def test_malformed_archives_and_expansion_limits_reject_before_storage(self):
        def malformed(name, size=0, kind=tarfile.REGTYPE, payload=b''):
            stream=io.BytesIO()
            with tarfile.open(fileobj=stream,mode='w') as archive:
                info=tarfile.TarInfo(name); info.size=size; info.type=kind
                archive.addfile(info,io.BytesIO(payload) if payload else None)
            return stream.getvalue()
        for raw in (b'not tar',malformed('../escape'),malformed('files/link',kind=tarfile.SYMTYPE),
                    malformed('enormous',size=129*1024*1024)):
            status,_=self.request('POST','/v1/souls',raw)
            self.assertIn(status,(400,413))
        self.assertEqual(list(self.server.store.blobs.iterdir()),[])


if __name__ == '__main__':
    # No inherited managed-Agent environment affects configuration/test endpoints.
    environment = sealed_environment()
    os.environ.clear()
    os.environ.update(environment)
    unittest.main()
