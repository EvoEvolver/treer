#!/usr/bin/env python3
"""Contract double for the installed `treer-agent` command.

`treer-agent session export|import ...` follows the native transfer CLI
(Claude only, explicit --config-dir); fixture supplies explicit source paths, no
native discovery. Any other argv is a normal Agent start and is echoed as JSON.
"""
import argparse
import base64
import hashlib
import json
import os
import sys
from pathlib import Path

if sys.argv[1:2]!=['session']:
    print(json.dumps({'argv':sys.argv[1:],'cwd':os.getcwd(),'claude_config':os.environ.get('CLAUDE_CONFIG_DIR')}))
    sys.exit(0)
if os.environ.get('FAKE_SESSION_LOG'):
    with open(os.environ['FAKE_SESSION_LOG'],'a') as log: log.write(json.dumps(sys.argv[1:])+'\n')
if sys.argv[2:]==['--help']:
    print('session export --harness claude --config-dir ABS --cwd ABS --session-id UUID\nsession import --input ABS --directory NEW_ABS [--session-id NEW_UUID]')
    sys.exit(0)
p=argparse.ArgumentParser()
p.add_argument('action')
for name in ('harness','config-dir','session-id','cwd','directory','input'):p.add_argument('--'+name)
a=p.parse_args(sys.argv[2:])
if a.action=='export':
    source=json.loads(Path(os.environ['FAKE_SESSION_SOURCE']).read_text())
    if a.harness!='claude' or not a.config_dir or not Path(a.config_dir).is_absolute():sys.exit(1)
    if a.harness!=source['harness'] or a.session_id!=source['sessionId']:sys.exit(1)
    files=[]
    for file in source['files']:
        path=Path(file['source'])
        if path.is_symlink():sys.exit(1)
        data=path.read_bytes()
        files.append(dict(scope=file['scope'],path=file['path'],encoding='base64',content=base64.b64encode(data).decode(),sha256=hashlib.sha256(data).hexdigest()))
    print(json.dumps(dict(schema='treer.agent-session/v1',harness=a.harness,sessionId=a.session_id,cwd=a.cwd,exportedAt='2026-10-12T00:00:00Z',files=files)))
elif a.action=='import':
    bundle=json.loads(Path(a.input).read_text())
    if bundle['harness']!='claude':sys.exit(1)
    root=Path(a.directory)
    root.mkdir(mode=0o700)
    cwd=root/'workspace';cwd.mkdir(mode=0o700)
    config=root/'claude';config.mkdir(mode=0o700)
    project=config/'projects'/'isolated';project.mkdir(parents=True,mode=0o700)
    for file in bundle['files']:
        data=base64.b64decode(file['content'])
        if file['scope']=='session' and file['path'].endswith('.jsonl'):
            records=[]
            for line in data.decode().splitlines():
                record=json.loads(line)
                if 'cwd' in record:record['cwd']=str(cwd)
                if 'sessionId' in record:record['sessionId']=a.session_id
                records.append(json.dumps(record)+'\n')
            (project/(a.session_id+'.jsonl')).write_text(''.join(records))
        elif file['scope']=='memory':
            target=project/'memory'/file['path'];target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(data)
        elif file['scope']=='project':(cwd/'CLAUDE.md').write_bytes(data)
    print(json.dumps(dict(sessionId=a.session_id,cwd=str(cwd),environment={'CLAUDE_CONFIG_DIR':str(config)})))
else:
    sys.exit(2)
