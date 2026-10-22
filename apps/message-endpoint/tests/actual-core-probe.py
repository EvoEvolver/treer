#!/usr/bin/env python3
"""Task-owned managed workload probe; credentials stay inherited, never serialized."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

root = Path(sys.argv[1])
role = sys.argv[2]

def write(name, value):
    with os.fdopen(os.open(root / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as f:
        json.dump(value, f)

def wait_file(name):
    deadline = time.monotonic() + 180
    while not (root / name).exists():
        if time.monotonic() > deadline:
            raise RuntimeError('probe_input_deadline')
        time.sleep(.1)
    return json.loads((root / name).read_text())

if role == 'relay':
    wait_file('relay-config.json')
    wait_file('relay-release.json')
    os.execv(sys.argv[3], [sys.argv[3], '--config', str(root/'relay-config.json')])

cli = str(root / 'bin/treer')
def call(argv, envelope=None):
    r = subprocess.run([cli, *argv], input=None if envelope is None else json.dumps(envelope), text=True,
                       capture_output=True, timeout=20)
    if r.returncode:
        raise RuntimeError('own_core_cli_rejected')
    return json.loads(r.stdout)

try:
    target = wait_file('send.json')['endpoint']
    argv = ['message','send','--to='+target,'--idempotency-key=actual204-original','--stdin-envelope']
    first = call(argv, {'body':'private core fixture body'})
    second = call(argv, {'body':'private core fixture body'})
    mid = first['message']['message_id']
    if second['message']['message_id'] != mid:
        raise RuntimeError('send_retry_changed_id')
    deadline = time.monotonic() + 150
    while time.monotonic() < deadline:
        batch = call(['message','receive','--limit','10','--wait','1000'])
        for delivery in batch['deliveries']:
            message = delivery['message']
            if message['sender']['id'] == target and mid in message['context_ids'] and 'actual member reply' in message['body']:
                call(['message','ack','--operation-id=actual204-reply-ack','--',delivery['delivery_id']])
                write('probe-result.json',dict(passed=True,actual_sender_endpoint=True,context_matched=True,
                    message_id=message['message_id'],source_message_id=mid,idempotent_send=True))
                while True:time.sleep(1)
        time.sleep(.1)
    raise RuntimeError('actual_reply_deadline')
except Exception as e:
    write('probe-result.json',dict(passed=False,failure_type=type(e).__name__,failure_code=str(e) if isinstance(e,RuntimeError) else 'redacted'))
    raise SystemExit(1)
