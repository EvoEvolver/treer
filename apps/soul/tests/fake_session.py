#!/usr/bin/env python3
"""Contract double: fixture supplies explicit source paths; no native discovery."""
import argparse
import base64
import hashlib
import json
import os
import sys
from pathlib import Path

p=argparse.ArgumentParser()
p.add_argument('action')
for name in ('harness','session-id','cwd','directory','input'):p.add_argument('--'+name)
a=p.parse_args()
if a.action=='export':
    source=json.loads(Path(os.environ['FAKE_SESSION_SOURCE']).read_text())
    if a.harness!=source['harness'] or a.session_id!=source['sessionId']:sys.exit(1)
    files=[]
    for file in source['files']:
        path=Path(file['source'])
        if path.is_symlink():sys.exit(1)
        data=path.read_bytes()
        files.append(dict(scope=file['scope'],path=file['path'],encoding='base64',content=base64.b64encode(data).decode(),sha256=hashlib.sha256(data).hexdigest()))
    print(json.dumps(dict(schema='treer.agent-session/v1',harness=a.harness,sessionId=a.session_id,cwd=a.cwd,exportedAt='2026-10-12T00:00:00Z',files=files)))
else:
    bundle=json.loads(Path(a.input).read_text())
    root=Path(a.directory)
    root.mkdir(mode=0o700)
    cwd=root/'workspace';cwd.mkdir(mode=0o700)
    config=root/bundle['harness'];config.mkdir(mode=0o700)
    project=config/'projects'/'isolated';project.mkdir(parents=True,mode=0o700)
    environment={'CODEX_HOME' if bundle['harness']=='codex' else 'CLAUDE_CONFIG_DIR':str(config)}
    for file in bundle['files']:
        data=base64.b64decode(file['content'])
        if file['scope']=='session' and file['path'].endswith('.jsonl'):
            records=[]
            for line in data.decode().splitlines():
                record=json.loads(line)
                if 'cwd' in record:record['cwd']=str(cwd)
                if 'sessionId' in record:record['sessionId']=a.session_id
                if record.get('type')=='session_meta':record['payload'].update(id=a.session_id,cwd=str(cwd))
                records.append(json.dumps(record)+'\n')
            target=(config/'sessions'/('imported-'+a.session_id+'.jsonl')) if bundle['harness']=='codex' else project/(a.session_id+'.jsonl')
            target.parent.mkdir(parents=True,exist_ok=True)
            target.write_text(''.join(records))
        elif file['scope']=='memory':
            target=project/'memory'/file['path'];target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(data)
        elif file['scope']=='project':(cwd/'CLAUDE.md').write_bytes(data)
    original=Path(os.environ['HOME'])/'.codex/auth.json'
    if bundle['harness']=='codex' and original.exists():(config/'auth.json').symlink_to(original)
    print(json.dumps(dict(sessionId=a.session_id,cwd=str(cwd),environment=environment)))
