#!/usr/bin/python3
"""Only cfg(test) includes this source. Synthetic loopback services; no real identity."""
import json, os, socket, sqlite3, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
root=Path(sys.argv[1]); root.chmod(0o700)
cli=root/'fixture-cli'
cli.write_text('''#!/usr/bin/python3
import json,sqlite3,sys,time
from pathlib import Path
p=Path(__file__).parent
own='ag_endpoint' if Path(__file__).name=='endpoint-cli' else 'app_hub'
args=sys.argv[1:]
db=sqlite3.connect(p/'core-fixture.sqlite3',timeout=2)
if args[:3]==['token','create','svc_hub']:
 if (p/'revoke-on-reserved-token').exists():
  with sqlite3.connect(p/'state'/'hub.sqlite3',timeout=2) as state:
   reserved=state.execute("SELECT EXISTS(SELECT 1 FROM email_dispatch WHERE state='dispatching')").fetchone()[0]
  if reserved:
   import urllib.request
   urllib.request.urlopen(urllib.request.Request((p/'fixture-origin').read_text()+'/fixture/revoke-member',data=b'{"revoked":true}',headers={'Content-Type':'application/json'}),timeout=2).read()
 print(json.dumps(dict(access_token='synthetic.app.signature',token_type='Bearer',audience='svc_hub',expires_in=300)))
elif args[:3]==['agent','show','--'] and args[3] in ['app_hub','ag_endpoint','ag_recipient']:
 print(json.dumps(dict(agent_id=args[3],workspace_id='ws_test',server_id='machine_fixture',kind='command',cwd='/private/fixture',status='running',path='/team/'+args[3])))
elif args[:2]==['message','receive']:
 time.sleep(.05)
 print(json.dumps(dict(deliveries=[json.loads(r[0]) for r in db.execute('SELECT payload FROM inbox WHERE ack=0 AND recipient=? ORDER BY id LIMIT 10',(own,))],remaining_unacknowledged=0)))
elif args[:2]==['message','ack']:
 db.execute('UPDATE inbox SET ack=1 WHERE delivery=? AND recipient=?',(args[-1],own));db.commit()
 print(json.dumps(dict(acknowledged_delivery_ids=[args[-1]],already_acknowledged_delivery_ids=[])))
elif args[:2]==['message','send']:
 assert '--stdin-envelope' in args
 to=next(a.split('=',1)[1] for a in args if a.startswith('--to='))
 assert (own,to) in [('app_hub','ag_endpoint'),('ag_endpoint','app_hub'),('ag_endpoint','ag_recipient')]
 envelope=json.load(sys.stdin);key=next(a for a in args if a.startswith('--idempotency-key='))
 key=own+key
 db.execute('INSERT OR IGNORE INTO sent(key,payload,sender,recipient,contexts) VALUES(?,?,?,?,?)',(key,json.dumps(envelope),own,to,json.dumps([a.split('=',1)[1] for a in args if a.startswith('--context=')])))
 n=db.execute('SELECT id FROM sent WHERE key=?',(key,)).fetchone()[0]
 message=dict(message_id='msg_fixture_'+str(n),sender=dict(kind='agent',id=own),body=envelope['body'],context_ids=[a.split('=',1)[1] for a in args if a.startswith('--context=')])
 delivery=dict(delivery_id='del_fixture_'+str(n),recipient=dict(kind='agent',id=to),message=message)
 if to!='ag_recipient':db.execute('INSERT OR IGNORE INTO inbox(delivery,payload,recipient) VALUES(?,?,?)',(delivery['delivery_id'],json.dumps(delivery),to))
 db.commit()
 print(json.dumps(dict(message=message)))
else: sys.exit(1)
'''); cli.chmod(0o700)
relay_cli=root/'endpoint-cli';relay_cli.write_bytes(cli.read_bytes());relay_cli.chmod(0o700)
db=sqlite3.connect(root/'core-fixture.sqlite3');db.executescript('CREATE TABLE inbox(id INTEGER PRIMARY KEY,delivery TEXT UNIQUE,payload TEXT,recipient TEXT NOT NULL,ack INTEGER DEFAULT 0);CREATE TABLE sent(id INTEGER PRIMARY KEY,key TEXT UNIQUE,payload TEXT,sender TEXT NOT NULL,recipient TEXT NOT NULL,contexts TEXT NOT NULL);');db.close()
lock=threading.Lock();updates=[];sent=[];operations=[];member_revoked=False;emails=[];email_uncertain=False;poll_trace=[];active_pollers=0;max_pollers=0
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*_):pass
 def reply(self,value,status=200):
  body=json.dumps(value).encode();self.send_response(status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 def do_GET(self):
  if self.path=='/api/apps/svc_hub/directory':return self.reply(dict(principals=[dict(id='user_owner',kind='human',role='owner')]+([] if member_revoked else [dict(id='user_member',kind='human',role='member')])))
  if self.path=='/fixture/status':
   with lock: result=dict(member_revoked=member_revoked,operations=list(operations),sent=list(sent),emails=list(emails),poll_trace=list(poll_trace),active_pollers=active_pollers,max_pollers=max_pollers)
   with sqlite3.connect(root/'core-fixture.sqlite3') as db:
    result['core_sent']=[json.loads(r[0]) for r in db.execute('SELECT payload FROM sent WHERE sender="app_hub" ORDER BY id')]
    result['core_messages']=[dict(id='msg_fixture_'+str(r[0]),sender=r[1],recipient=r[2],envelope=json.loads(r[3]),context_ids=json.loads(r[4])) for r in db.execute('SELECT id,sender,recipient,payload,contexts FROM sent ORDER BY id')]
    result['acked']=db.execute('SELECT COUNT(*) FROM inbox WHERE ack=1').fetchone()[0]
   return self.reply(result)
  return self.reply({},404)
 def do_POST(self):
  global member_revoked,email_uncertain,active_pollers,max_pollers
  size=int(self.headers.get('Content-Length','0'));assert size<1024*1024
  value=json.loads(self.rfile.read(size))
  if self.path=='/.treer/apps/identity/verify':
   assert value.get('audience')=='svc_hub'
   if value.get('token') not in ('synthetic.owner.token159','synthetic.member.token159'):return self.reply(dict(active=False))
   return self.reply(dict(active=True,claims=dict(sub='user_member' if value['token']=='synthetic.member.token159' else 'user_owner',principal_kind='human',workspace_id='ws_test',service_id='svc_hub',exp=int(time.time())+300)))
  if self.path=='/fixture/email-uncertain':
   email_uncertain=bool(value['uncertain']);return self.reply({})
  if self.path=='/api/apps/svc_hub/email':
   assert self.headers.get('Authorization')=='Bearer synthetic.app.signature'
   with lock:emails.append(value)
   return self.reply(dict(status='in_progress') if email_uncertain else dict(status='sent',email_id='email_fixture'),409 if email_uncertain else 200)
  if self.path=='/fixture/revoke-member':
   member_revoked=bool(value['revoked']);return self.reply({})
  if self.path=='/fixture/update':
   with lock:updates.append(value)
   return self.reply({})
  if self.path=='/fixture/core':
   with sqlite3.connect(root/'core-fixture.sqlite3') as db:db.execute('INSERT INTO inbox(delivery,payload,recipient) VALUES(?,?,?)',(value['delivery_id'],json.dumps(value),value['recipient']['id']))
   return self.reply({})
  if not self.path.startswith('/TREER_TEST_ONLY_BOT_ORIGIN_159/bot12345:'):
   return self.reply({},404)
  operation=self.path.rsplit('/',1)[1]
  if operation=='getUpdates':
   with lock:
    operations.append(operation);active_pollers+=1;max_pollers=max(max_pollers,active_pollers)
    poll_trace.append(dict(actor=self.headers.get('X-Fixture-Poller','hub'),offset=value['offset']))
   try:
    time.sleep(0.04)
    with lock:result=[u for u in updates if u['update_id']>=value['offset']]
   finally:
    with lock:active_pollers-=1
   return self.reply(dict(ok=True,result=result))
  with lock:
   operations.append(operation)
   if operation=='getMe':result=dict(id=12345,is_bot=True)
   elif operation=='getUpdates':result=[u for u in updates if u['update_id']>=value['offset']]
   elif operation=='sendMessage':
    assert value['chat_id']==-100 and value.get('parse_mode') is None
    sent.append(value);result=dict(message_id=len(sent)+100,chat=dict(id=value['chat_id']))
   else:raise AssertionError('Unexpected Bot API operation')
  self.reply(dict(ok=True,result=result))
server=ThreadingHTTPServer(('127.0.0.1',0),Handler);server.daemon_threads=True
(root/'fixture-origin').write_text('http://127.0.0.1:'+str(server.server_port))
thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
with socket.socket() as reservation:
 reservation.bind(('127.0.0.1',0));port=reservation.getsockname()[1]
print(json.dumps(dict(cli=str(cli),proxy_url='http://127.0.0.1:'+str(server.server_port),listen='127.0.0.1:'+str(port),public_url='http://127.0.0.1:'+str(port)+'/',bot_origin='http://127.0.0.1:'+str(server.server_port)+'/TREER_TEST_ONLY_BOT_ORIGIN_159')),flush=True)
try:sys.stdin.readline()
finally:server.shutdown();server.server_close();thread.join()
