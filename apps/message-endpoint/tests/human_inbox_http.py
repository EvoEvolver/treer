#!/usr/bin/env python3
"""Closed native Hub + SQLite + synthetic loopback identity service; no real channel."""
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import time

from http_fixture import fixture, MEMBER, TOKEN, THREAD


def run(binary):
    checks = []
    def check(name, condition):
        if not condition:
            raise AssertionError(name)
        checks.append(name)
    with fixture(binary, editable=True, email=True) as f:
        headers = {'X-Treer-Identity-Token': MEMBER}
        def get(path, identity=headers):
            status, head, raw = f['request'](path, identity)
            check('private_response_no_store', head.get('cache-control') == 'no-store')
            return status, json.loads(raw)
        check('no_identity_refused', get('/api/email/inbox', {})[0] == 401)
        status, data = get('/api/email/inbox')
        check('member_index', status == 200 and len(data['threads']) == 1 and not data['truncated'])
        check('index_target', data['threads'][0]['id'] == THREAD and data['threads'][0]['recipient_agent_id'] == 'ag_recipient')
        check('metadata_only', 'confidential' not in json.dumps(data) and 'person@example.test' not in json.dumps(data))
        check('owner_not_implicit_member', get('/api/email/inbox', {'X-Treer-Identity-Token': TOKEN})[1]['threads'] == [])
        check('query_selector_refused', get('/api/email/inbox?member=user_owner')[0] == 400)
        status, thread = get('/api/email/thread/' + THREAD)
        check('selected_thread_read', status == 200 and thread['thread']['id'] == THREAD)
        mutation = dict(headers, Origin=f['url'].rstrip('/'), **{'Content-Type':'application/json', 'X-Endpoint-Request':'1', 'Sec-Fetch-Site':'same-origin'})
        payload = json.dumps({'key':'member-inbox-explicit-retry', 'body':'Synthetic member reply; no real channel'})
        for _ in range(2):
            status, _, raw = f['request']('/api/email/thread/' + THREAD, mutation, 'POST', payload)
            check('reply_queued', status == 202 and json.loads(raw)['status'] == 'queued')
        status, data = get('/api/email/inbox')
        check('reply_updates_index', status == 200 and data['threads'][0]['message_count'] == 2)
        db = sqlite3.connect(f['state'] / 'hub.sqlite3')
        check('identical_retry_single_job', db.execute('SELECT count(*) FROM email_replies').fetchone()[0] == 1 and db.execute('SELECT count(*) FROM inbound').fetchone()[0] == 1)
        db.close()
        f['stop']()
        env = {'HOME':str(f['root']), 'TMPDIR':str(f['root']), 'PATH':'/usr/bin:/bin:/usr/sbin:/sbin', 'TREER_AGENT_ID':'app_hub', 'TREER_WORKSPACE_ID':'ws_test'}
        resumed = subprocess.Popen([str(binary), '--config',str(f['root']/'config.json')], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            for _ in range(100):
                if resumed.poll() is not None:
                    raise AssertionError('restart_exited')
                try:
                    if f['request']('/health')[0] == 200:
                        break
                except OSError:
                    time.sleep(.05)
            else:
                raise AssertionError('restart_timeout')
            status, data = get('/api/email/inbox')
            check('restart_preserves_thread_and_reply', status == 200 and data['threads'][0]['message_count'] == 2)
            check('paused_history_readable', get('/api/email/thread/' + THREAD)[0] == 200)
        finally:
            if resumed.poll() is None:
                resumed.terminate()
                try:
                    resumed.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    resumed.kill()
                    resumed.wait(timeout=3)
        check('owned_restart_process_exited', resumed.poll() is not None)
    return {'status':'PASS','checks':checks,'scope':'native Hub/SQLite; synthetic loopback identity; no real Core or external send','owned_processes_finished':True}

if __name__ == '__main__':
    print(json.dumps(run(Path(sys.argv[1]).resolve())))
