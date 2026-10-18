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
args=sys.argv[1:]
db=sqlite3.connect(p/'core-fixture.sqlite3',timeout=2)
if args[:3]==['agent','show','--'] and args[3] in ['ag_endpoint','ag_recipient']:
 print(json.dumps(dict(agent_id=args[3],workspace_id='ws_test',server_id='machine_fixture',kind='command',cwd='/private/fixture',status='running',path='/team/'+args[3])))
elif args[:2]==['message','receive']:
 time.sleep(.05)
 print(json.dumps(dict(deliveries=[json.loads(r[0]) for r in db.execute('SELECT payload FROM inbox WHERE ack=0 ORDER BY id LIMIT 10')],remaining_unacknowledged=0)))
elif args[:2]==['message','ack']:
 db.execute('UPDATE inbox SET ack=1 WHERE delivery=?',(args[-1],));db.commit()
 print(json.dumps(dict(acknowledged_delivery_ids=[args[-1]],already_acknowledged_delivery_ids=[])))
elif args[:2]==['message','send']:
 assert '--to=ag_endpoint' in args and '--stdin-envelope' in args
 envelope=json.load(sys.stdin);key=next(a for a in args if a.startswith('--idempotency-key='))
 db.execute('INSERT OR IGNORE INTO sent(key,payload) VALUES(?,?)',(key,json.dumps(envelope)));db.commit()
 n=db.execute('SELECT id FROM sent WHERE key=?',(key,)).fetchone()[0]
 print(json.dumps(dict(message=dict(message_id='msg_fixture_'+str(n),sender=dict(kind='agent',id='app_hub'),body=envelope['body'],context_ids=[]))))
else: sys.exit(1)
'''); cli.chmod(0o700)
db=sqlite3.connect(root/'core-fixture.sqlite3');db.executescript('CREATE TABLE inbox(id INTEGER PRIMARY KEY,delivery TEXT UNIQUE,payload TEXT,ack INTEGER DEFAULT 0);CREATE TABLE sent(id INTEGER PRIMARY KEY,key TEXT UNIQUE,payload TEXT);');db.close()
lock=threading.Lock();updates=[];sent=[];operations=[]
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*_):pass
 def reply(self,value,status=200):
  body=json.dumps(value).encode();self.send_response(status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 def do_GET(self):
  if self.path=='/api/apps/svc_hub/directory':return self.reply(dict(principals=[dict(id='user_owner',kind='human',role='owner')]))
  if self.path=='/fixture/status':
   with lock: result=dict(operations=list(operations),sent=list(sent))
   with sqlite3.connect(root/'core-fixture.sqlite3') as db:result['core_sent']=[json.loads(r[0]) for r in db.execute('SELECT payload FROM sent ORDER BY id')];result['acked']=db.execute('SELECT COUNT(*) FROM inbox WHERE ack=1').fetchone()[0]
   return self.reply(result)
  return self.reply({},404)
 def do_POST(self):
  size=int(self.headers.get('Content-Length','0'));assert size<1024*1024
  value=json.loads(self.rfile.read(size))
  if self.path=='/.treer/apps/identity/verify':
   assert value==dict(token='synthetic.owner.token159',audience='svc_hub')
   return self.reply(dict(active=True,claims=dict(sub='user_owner',principal_kind='human',workspace_id='ws_test',service_id='svc_hub',exp=int(time.time())+300)))
  if self.path=='/fixture/update':
   with lock:updates.append(value)
   return self.reply({})
  if self.path=='/fixture/core':
   with sqlite3.connect(root/'core-fixture.sqlite3') as db:db.execute('INSERT INTO inbox(delivery,payload) VALUES(?,?)',(value['delivery_id'],json.dumps(value)))
   return self.reply({})
  if not self.path.startswith('/TREER_TEST_ONLY_BOT_ORIGIN_159/bot12345:'):
   return self.reply({},404)
  operation=self.path.rsplit('/',1)[1]
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
thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
with socket.socket() as reservation:
 reservation.bind(('127.0.0.1',0));port=reservation.getsockname()[1]
print(json.dumps(dict(cli=str(cli),proxy_url='http://127.0.0.1:'+str(server.server_port),listen='127.0.0.1:'+str(port),public_url='http://127.0.0.1:'+str(port)+'/',bot_origin='http://127.0.0.1:'+str(server.server_port)+'/TREER_TEST_ONLY_BOT_ORIGIN_159')),flush=True)
try:sys.stdin.readline()
finally:server.shutdown();server.server_close();thread.join()
