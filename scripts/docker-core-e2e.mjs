// Local-only regression: Node 20+, Docker, no provider credentials or Railway.
// Build current source first: docker build -t treer-local-e2e:fixed .
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import http from 'node:http';
import assert from 'node:assert/strict';

const exec = promisify(execFile);
const root = fileURLToPath(new URL('../', import.meta.url));
const fixtures = path.join(root, 'scripts/docker-core-e2e');
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'treer-core-e2e-'));
const prefix = `treer-core-${path.basename(dir).slice(-6).toLowerCase()}`;
const image = process.env.TREER_E2E_IMAGE || 'treer-local-e2e:fixed';
const names = suffix => `${prefix}-${suffix}`;
const containers = [];
const results = [];
let base, cookie = '', ws;
const docker = async (...args) => (await exec('docker', args, {timeout:120000, maxBuffer:8*1024*1024})).stdout.trim();
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function wait(label, fn) {
  const deadline = Date.now() + 60000;
  let error;
  while (Date.now() < deadline) {
    try { const value = await fn(); if (value) return value; } catch (e) { error = e; }
    await sleep(500);
  }
  throw new Error(`timeout ${label}: ${error?.message || ''}`);
}
async function check(label, fn) {
  const start = Date.now();
  try {
    await fn();
    results.push({label, status:'PASS', ms:Date.now()-start});
    console.log('PASS', label);
  } catch (error) {
    results.push({label, status:'FAIL', error:error.message});
    throw error;
  } finally { fs.writeFileSync(path.join(dir,'results.json'), JSON.stringify(results,null,2)); }
}
async function run(suffix, ...args) {
  const name = names(suffix);
  await docker('run','-d','--name',name,'--network',names('net'),...args);
  containers.push(name);
}
async function api(route, body, options={}) {
  const headers = {Cookie:cookie, ...options.headers};
  const payload = body === undefined ? undefined : JSON.stringify(body);
  if (payload !== undefined) {
    headers['Content-Type'] = 'application/json';
    headers['Content-Length'] = Buffer.byteLength(payload);
  }
  const response = await new Promise((resolve,reject) => {
    // node:http preserves Host; fetch may replace it with the request URL host.
    const req = http.request(base+route, {method:options.method || (body===undefined?'GET':'POST'),headers}, res => {
      const chunks=[];
      res.on('data',chunk=>chunks.push(chunk));
      res.on('end',()=>resolve({status:res.statusCode,headers:res.headers,text:Buffer.concat(chunks).toString()}));
    });
    req.setTimeout(35000,()=>req.destroy(new Error('HTTP timeout')));
    req.on('error',reject);
    req.end(payload);
  });
  try { response.data = JSON.parse(response.text); } catch { response.data = response.text; }
  if (!options.raw && (response.status<200 || response.status>=300)) throw new Error(`${route}: ${response.status} ${response.text}`);
  return response;
}
function sessionCookie(response) { return response.headers['set-cookie'][0].split(';')[0]; }
async function cli(side, ...argv) {
  // Exercise installed configuration discovery directly; no credential wrapper.
  const out = await docker('exec',names('machine-'+side),'treer','--workspace',ws,...argv);
  try { return JSON.parse(out); } catch { return out; }
}
async function command(agent, label, argv, extra={}) {
  await api(`/api/workspaces/${ws}/agents/${agent}/prompt`,{text:JSON.stringify({label,argv,...extra})});
  const result = await wait(label,async()=>{
    const text = (await api(`/api/workspaces/${ws}/agents/${agent}/output?lines=300`)).data.text;
    const line = text.split(/\r?\n/).find(line=>line.startsWith('RESULT ') && line.includes(`"label": "${label}"`));
    return line && JSON.parse(line.slice(7));
  });
  assert.equal(result.returncode,0,JSON.stringify(result));
  try { return JSON.parse(result.stdout); } catch { return result; }
}
const snapshot = async () => (await api(`/api/workspaces/${ws}/snapshot`)).data;
const appInfo = async id => (await api(`/api/workspaces/${ws}/apps/${id}`)).data.app;
const agents = {}, services = {}, apps = {};
const recovery = {};
const proxyEntries = {};
const probe = (agent, label, mode, target) => command(agent,label,[],{program:['python3','/fixtures/proxy_probe.py',mode,target]});
let networkCreated = false;
console.log(`Evidence: ${dir}\nDocker prefix: ${prefix}\nImage: ${image}`);
try {
  await docker('image','inspect',image);
  await docker('build','-t',names('fixture'),fixtures);
  await docker('network','create',names('net')); networkCreated = true;
  await run('pg','--network-alias','pg','-e','POSTGRES_PASSWORD=e2e-only','-e','POSTGRES_USER=treer','-e','POSTGRES_DB=treer','postgres:17-alpine');
  await run('nats','--network-alias','nats','nats:2.14.4-alpine3.22','-js');
  await wait('postgres',()=>docker('exec',names('pg'),'pg_isready','-U','treer'));
  for (const side of ['a','b']) {
    await run('proxy-'+side,'--init','--network-alias','proxy-'+side,'-p','127.0.0.1::8787',
      '-e','ADMIN_PASSWORD=e2e-admin','-e','DATABASE_URL=postgres://treer:e2e-only@pg:5432/treer','-e','PORT=8787',
      '-e',`TREER_PROXY_PUBLIC_URL=http://proxy-${side}:8787/`,'-e','TREER_APP_PUBLIC_URL=http://web.invalid/',
      '-e','TREER_INGRESS_PUBLIC_URL=http://apps.treer.test:8787/','-e','TREER_ENABLE_CORE_MESSAGES=true',
      '-e','TREER_NATS_URL=nats://nats:4222','-e','TREER_PUBLIC_TCP_PORT_RANGE=19200-19201',image);
    const url = 'http://'+await docker('port',names('proxy-'+side),'8787/tcp');
    await wait('proxy '+side,async()=> (await fetch(url+'/api/health')).ok);
    if (side==='a') base=url;
    await run('machine-'+side,'--init','--mount',`type=bind,src=${fixtures},dst=/fixtures,readonly`,names('fixture'));
  }
  await check('real authentication, invitation, registration, workspace',async()=>{
    assert.equal((await api('/api/organizations',undefined,{raw:true})).status,401);
    const admin = sessionCookie(await api('/api/admin/login',{password:'e2e-admin'}));
    const invite = (await api('/api/admin/invitations',{}, {headers:{Cookie:admin}})).data.token;
    cookie = sessionCookie(await api('/api/auth/register',{invite,email:'local@treer.invalid',preferred_name:'Local E2E',password:'e2e-user-password'}));
    const organization_id = (await api('/api/organizations')).data.organizations[0].organization_id;
    ws = (await api('/api/workspaces',{organization_id,name:'Local Docker regression'})).data.workspace.workspace_id;
  });
  for (const side of ['a','b']) await check(`install/preflight/enroll ${side}; key survives missing ps`,async()=>{
    const machine=names('machine-'+side);
    const installed = await docker('exec',machine,'sh','-c',`curl -fsSL http://proxy-${side}:8787/install.sh | sh`);
    assert(!installed.includes('unshare'));
    const key = (await api(`/api/workspaces/${ws}/bootstrap`,{})).data.enrollment_key;
    const connect = ['exec',machine,'treer-agent-server','connect','--key',key,'--proxy',`http://proxy-${side}:8787/`,'--non-interactive','--accept-risk','--name','docker-'+side];
    await assert.rejects(docker(...connect),error=>error.message.includes('procps'));
    assert.equal((await snapshot()).servers.length,side==='a'?0:1);
    await docker('exec',machine,'sh','-c','test ! -d "$HOME/.config/treer/agent-servers"');
    await docker('exec','-u','root',machine,'sh','-c','apt-get update -qq && apt-get install -y -qq --no-install-recommends procps');
    await docker(...connect); // The exact same one-time key must still work.
    assert((await cli(side,'status')).servers.some(s=>s.name==='docker-'+side&&s.status==='online'));
    const automatic = JSON.parse(await docker('exec',machine,'treer','status'));
    assert(automatic.servers.some(s=>s.name==='docker-'+side));
    assert((await cli(side,'message','--help')).includes('durable'));
    assert(!(await cli(side,'message','send','--help')).includes('--body-file'));
    assert((await cli(side,'--skill')).includes('Exchange durable Messages'));
  });
  for (const [side,port] of [['a',8200],['b',8300]]) await check(`Agent identity/startup ${side}`,async()=>{
    const agent=await cli(side,'agent','admin','create','--machine','docker-'+side,'--kind','command','--name','worker-'+side,'--','python3','/fixtures/worker.py',side,String(port));
    agents[side]=(agent.agent??agent).agent_id;
    await wait('worker '+side,async()=> (await api(`/api/workspaces/${ws}/agents/${agents[side]}/output`)).data.text.includes('READY '+side));
    const who=await command(agents[side],'who-'+side,['whoami']); assert.equal(who.agent.agent_id,agents[side]);
    assert((await command(agents[side],'startup-'+side,['agent','startup','show'])).enabled);
  });
  await check('cross-Proxy exec, discovery and literal argv',async()=>{
    assert((await command(agents.a,'discover',['status'])).agents.some(a=>a.agent_id===agents.b));
    const literal=['中文','space value','$(literal)','--url','literal-url'];
    const out=await command(agents.a,'exec',['exec','--machine','docker-b','--','python3','-c','import json,sys;print(json.dumps(sys.argv[1:]))',...literal]);
    assert.deepEqual(JSON.parse(out.stdout),literal);
  });
  await check('durable Message stdin, legacy stdin alias, idempotency, ack and reply',async()=>{
    const body='多行正文\n$literal\n';
    const argv=['message','send','--to',agents.b,'--idempotency-key','local-message'];
    const sent=await command(agents.a,'send',argv,{stdin:body}); assert.equal(sent.message.body,body);
    const again=await command(agents.a,'resend',[...argv,'--body-file','-'],{stdin:body});
    assert(again.idempotent_replay); assert.equal(again.message.message_id,sent.message.message_id);
    const one=await command(agents.b,'receive',['message','receive']);
    const two=await command(agents.b,'redeliver',['message','receive']);
    assert.equal(one.deliveries[0].delivery_id,two.deliveries[0].delivery_id);
    await command(agents.b,'ack',['message','ack',one.deliveries[0].delivery_id,'--operation-id','local-ack']);
    assert.equal((await command(agents.b,'empty',['message','receive'])).deliveries.length,0);
    const reply=await command(agents.b,'reply',['message','reply',sent.message.message_id,'--body','reply','--idempotency-key','local-reply']);
    assert.deepEqual(reply.message.context_ids,[sent.message.message_id]);
  });
  await check('Profile launch resolves machine name and separates credentials',async()=>{
    const code="import os,sys;assert not os.environ.get('TREER_OPERATOR_CREDENTIAL');assert os.environ['TREER_WORKLOAD_CREDENTIAL'];print('PROFILE_OK',repr(sys.argv[1:]))";
    await command(agents.a,'profile-create',['agent','admin','profile','create','local-profile','python3','--','-c',code,'中文','$(literal)']);
    const launch=await command(agents.a,'profile-launch',['agent','admin','profile','launch','local-profile','--machine','docker-b','--name','profile-agent']);
    const id=(launch.agent??launch).agent_id;
    const text=await wait('profile',async()=>{const t=(await api(`/api/workspaces/${ws}/agents/${id}/output`)).data.text;return t.includes('PROFILE_OK')&&t;});
    assert(text.includes('$(literal)')); assert(text.includes('中文'));
    await assert.rejects(docker('exec','-e','TREER_IDENTITY_MODE=agent',names('machine-a'),'treer','status'),e=>e.message.includes('operator fallback is disabled'));
  });
  await check('private HTTP and binary TCP forwards across Proxies',async()=>{
    for (const [type,port] of [['http',8200],['tcp',8201]]) {
      const registered=await command(agents.a,'register-'+type,['service','register','--name','local-'+type,'--port',String(port),'--protocol',type]);
      services[type]=registered.service.service_id;
      await command(agents.b,'forward-'+type,['service','forward',services[type],'--listen',`127.0.0.1:${port+200}`],{background:true});
    }
    assert((await wait('HTTP forward',()=>docker('exec',names('machine-b'),'curl','--max-time','3','-fsS','http://127.0.0.1:8400/asset'))).includes('treer-e2e-a:/asset'));
    const echo="import socket\ns=socket.create_connection(('127.0.0.1',8401),timeout=10);s.settimeout(10)\ndata=bytes(range(256))*4096;s.sendall(data);s.shutdown(socket.SHUT_WR)\nout=bytearray()\nwhile True:\n b=s.recv(65536)\n if not b:break\n out.extend(b)\nassert out==data;print(len(out))";
    assert.equal(await wait('TCP forward',()=>docker('exec',names('machine-b'),'python3','-c',echo)),'1048576');
  });
  await check('per-Agent HTTP entries inherit into code and sign the correct identity',async()=>{
    for (const side of ['a','b']) {
      const result=await probe(agents[side],'proxy-identity-'+side,'http',`http://${services.http}.treer/identity`);
      proxyEntries[side]=result.proxy;
      assert.equal(result.response.claims.sub,agents[side]);
      assert.equal(result.response.claims.aud,services.http);
      assert.equal(result.response.claims.workspace_id,ws);
      assert.equal(result.response.bearer_matches,true);
      assert.equal(result.response.leaked,false);
      const again=await probe(agents[side],'proxy-fresh-'+side,'http',`http://${services.http}.treer/identity`);
      assert.notEqual(again.response.claims.jti,result.response.claims.jti);
    }
  });
  await check('proxy CONNECT binary half-close and real HTTPS with certificate verification',async()=>{
    for (const [label,target] of [['id',services.tcp],['name','LOCAL-TCP']]) {
      const result=await probe(agents.b,'proxy-connect-'+label,'connect',`${target}.treer:22`);
      assert.equal(result.bytes,1048576);
    }
    const tls=await command(agents.b,'proxy-https',[],{program:['curl','--noproxy','','--cacert','/tmp/treer-e2e-cert.pem','--max-time','10','-fsS','https://127.0.0.1:8302/tls']});
    assert(tls.stdout.includes('treer-e2e-b:/tls'));
  });
  await check('service names resolve case-insensitively and follow rename without changing identity',async()=>{
    const before=await probe(agents.b,'proxy-name-before','http','http://LOCAL-HTTP.treer/identity');
    assert.equal(before.response.claims.aud,services.http);assert.equal(before.response.claims.sub,agents.b);
    await api(`/api/workspaces/${ws}/services/${services.http}`,{name:'renamed-http'},{method:'PATCH'});
    const after=await probe(agents.b,'proxy-name-after','http','http://renamed-http.treer/identity');
    assert.equal(after.response.claims.aud,services.http);assert.equal(after.response.claims.sub,agents.b);
    const code="import json,urllib.request,urllib.error\ntry: urllib.request.urlopen('http://local-http.treer/identity');raise Exception('old name still resolved')\nexcept urllib.error.HTTPError as e: print(json.dumps({'status':e.code}))";
    assert.equal((await command(agents.b,'proxy-old-name',[],{program:['python3','-c',code]})).status,404);
    const stable=await probe(agents.b,'proxy-stable-id','http',`http://${services.http}.treer/identity`);
    assert.equal(stable.response.claims.aud,services.http);
  });
  await check('stopped Agent proxy entry is removed',async()=>{
    const code="import os,time;print('PROXY_ENTRY '+os.environ['http_proxy'],flush=True);time.sleep(3600)";
    const created=await cli('a','agent','admin','create','--machine','docker-a','--kind','command','--name','proxy-stop','--','python3','-c',code);
    const id=(created.agent??created).agent_id;
    const entry=await wait('stop fixture proxy',async()=>{
      const text=(await api(`/api/workspaces/${ws}/agents/${id}/output`)).data.text;
      return text.match(/^PROXY_ENTRY (http:\/\/127\.0\.0\.1:\d+)/m)?.[1];
    });
    assert.notEqual(entry,proxyEntries.a);
    await cli('a','agent','admin','stop',id);
    await wait('entry removed',()=>docker('exec',names('machine-a'),'python3','-c',`import socket;assert socket.socket().connect_ex(('127.0.0.1',${new URL(entry).port}))!=0;print('closed')`));
  });
  await check('HTTP publish/access/unpublish uses installed CLI throughout',async()=>{
    const ingress=(await command(agents.a,'publish',['service','publish',services.http,'--access','public'])).ingress;
    const headers={Cookie:'',Host:new URL(ingress.url).host};
    const res=await api('/asset',undefined,{raw:true,headers}); assert.equal(res.status,200);assert.equal(res.data,'treer-e2e-a:/asset');
    await command(agents.a,'unpublish',['service','unpublish',services.http]);
    assert.equal((await api('/asset',undefined,{raw:true,headers})).status,404);
  });
  await check('updated Canary shell command works with explicit forwarding',async()=>{
    const canary=fs.readFileSync(path.join(root,'scripts/test-canary.sh'),'utf8');
    const match=canary.match(/forward_command='([\s\S]*?)'\nfor attempt/); assert(match);
    const shell=match[1].split("'\\''").join("'");
    const server=(await snapshot()).servers.find(s=>s.name==='docker-b').server_id;
    const created=(await api(`/api/workspaces/${ws}/agents`,{server_id:server,kind:'command',name:'canary-forward',cwd:'',args:['sh','-c',shell,'local-canary',services.http]})).data;
    const id=(created.agent??created).agent_id; assert(id);
    await wait('Canary forward output',async()=> (await api(`/api/workspaces/${ws}/agents/${id}/output`)).data.text.includes('treer-e2e-a:/'));
    await api(`/api/workspaces/${ws}/agents/${id}`,undefined,{method:'DELETE'});
  });
  for (const [name,port,isPublic] of [['public',8500,true],['private',8502,false]]) await check(`deploy ${name} App from A to B`,async()=>{
    const result=await command(agents.a,'app-'+name,['app','create','--machine','docker-b','--name',name,'--port',String(port),...(isPublic?['--public']:[]),'python3','--','/fixtures/app.py',String(port)]);
    apps[name]=result.app;
    await wait(name+' App',async()=> (await appInfo(apps[name].app_id)).status==='running');
  });
  await check('Managed App has its own entry and HTTP identity; WebSocket upgrades pass through',async()=>{
    const result=await probe(agents.a,'app-proxy','http',`http://public.treer/proxy?service=${services.http}`);
    const app=result.response;
    assert.equal(app.claims.sub,app.runtime_id);
    assert.notEqual(app.runtime_id,agents.b);
    assert.notEqual(app.proxy,proxyEntries.b);
    proxyEntries.app=app.proxy;
    assert.equal(app.claims.aud,services.http);
    assert.equal(app.leaked,false);
    assert.equal((await probe(agents.a,'proxy-ws','ws','http://public.treer/ws')).websocket,true);
  });
  await check('public App HTTP/redirect/Unicode POST; private App auth callback',async()=>{
    const publicHeaders={Cookie:'',Host:new URL(apps.public.public_url).host};
    assert.equal((await api('/',undefined,{raw:true,headers:publicHeaders})).status,200);
    assert.equal((await api('/redirect',undefined,{raw:true,headers:publicHeaders})).headers.location,'/asset');
    assert.equal((await api('/echo','中文',{raw:true,headers:publicHeaders})).data,'中文');
    const host=new URL(apps.private.public_url).host;
    const denied=await api('/',undefined,{raw:true,headers:{Cookie:'',Host:host}});assert.equal(denied.status,303);
    const authUrl=new URL(denied.headers.location);
    const auth=await api(authUrl.pathname+authUrl.search,undefined,{raw:true,headers:{Host:authUrl.host}});assert.equal(auth.status,303);
    const cb=new URL(auth.headers.location);
    const callback=await api(cb.pathname+cb.search,undefined,{raw:true,headers:{Cookie:'',Host:host}});assert.equal(callback.status,303);
    assert.equal((await api('/',undefined,{raw:true,headers:{Cookie:sessionCookie(callback),Host:host}})).status,200);
    assert.equal((await api(cb.pathname+cb.search,undefined,{raw:true,headers:{Cookie:'',Host:host}})).status,401);
  });
  await check('App stop/start/restart and crash recovery retain service and URL',async()=>{
    const id=apps.public.app_id;
    await command(agents.a,'stop-public',['app','stop',id]);
    await wait('public stopped',async()=> (await appInfo(id)).status==='stopped');
    await wait('App entry removed',()=>docker('exec',names('machine-b'),'python3','-c',`import socket;assert socket.socket().connect_ex(('127.0.0.1',${new URL(proxyEntries.app).port}))!=0;print('closed')`));
    await command(agents.a,'start-public',['app','start',id]);
    const started=await wait('public started',async()=>{const a=await appInfo(id);return a.status==='running'&&a;});
    await command(agents.a,'restart-public',['app','restart',id]);
    const restarted=await wait('public restarted',async()=>{const a=await appInfo(id);return a.status==='running'&&a.pid!==started.pid&&a;});
    await docker('exec',names('machine-b'),'kill','-KILL',String(restarted.pid));
    const recovered=await wait('public crash recovery',async()=>{const a=await appInfo(id);return a.status==='running'&&a.pid!==restarted.pid&&a;});
    assert.equal(recovered.service_id,apps.public.service_id);assert.equal(recovered.public_url,apps.public.public_url);
    await wait('crash-recovered HTTP',async()=> (await api('/',undefined,{raw:true,headers:{Cookie:'',Host:new URL(recovered.public_url).host}})).status===200);
    const restored=(await probe(agents.a,'app-proxy-crash-restored','http',`http://${recovered.service_id}.treer/proxy?service=${services.http}`)).response;
    assert.equal(restored.claims.sub,restored.runtime_id);
  });
  await check('hot Controller restart and repeated restore retain Agent process',async()=>{
    const before=(await snapshot()).agents.find(a=>a.agent_id===agents.a);
    await docker('exec',names('machine-a'),'treer-agent-server','service','--workspace',ws,'restart-controller');
    await wait('Controller restart',()=>cli('a','status'));
    for (let i=0;i<2;i++) assert((await cli('a','runtime','restore','--all')).items.some(item=>item.workload_id===agents.a&&item.state==='attached'));
    const after=(await snapshot()).agents.find(a=>a.agent_id===agents.a);assert.equal(after.pid,before.pid);assert.equal(after.started_at,before.started_at);
    const resumed=await probe(agents.a,'proxy-hot-restored','http','http://renamed-http.treer/identity');
    assert.equal(resumed.proxy,proxyEntries.a);assert.equal(resumed.response.claims.sub,agents.a);
  });
  await check('prepare stopped, completed and invalid-startup recovery cases',async()=>{
    for (const [name,argv] of [['stopped',['sleep','3600']],['completed',['sh','-c','echo done']],['invalid',['python3','/fixtures/recovery_failure.py']]]) {
      const created=await cli('a','agent','admin','create','--machine','docker-a','--kind','command','--name',name,'--',...argv);
      recovery[name]=(created.agent??created).agent_id;
    }
    await cli('a','agent','admin','stop',recovery.stopped);
    await wait('completed Agent',async()=> (await snapshot()).agents.find(a=>a.agent_id===recovery.completed)?.status==='exited');
    await wait('invalid startup saved',async()=> (await api(`/api/workspaces/${ws}/agents/${recovery.invalid}/output`)).data.text.includes('STARTUP_BAD 0'));
  });
  await check('cold machine restore retains Agents, services, App IDs/URLs and stopped state',async()=>{
    await command(agents.a,'stop-private',['app','stop',apps.private.app_id]);
    for (const side of ['a','b']) {
      const before=(await snapshot()).agents.find(a=>a.agent_id===agents[side]);
      await docker('stop','-t','2',names('machine-'+side));await docker('start',names('machine-'+side));
      await docker('exec',names('machine-'+side),'treer-agent-server','restore','--workspace',ws);
      await wait('Agent restored '+side,async()=>{const a=(await snapshot()).agents.find(a=>a.agent_id===agents[side]);return a&&a.started_at!==before.started_at&&!['failed','exited'].includes(a.status);});
      await cli(side,'runtime','restore','--all');
    }
    const app=await wait('App restored',async()=>{const a=await appInfo(apps.public.app_id);return a.status==='running'&&a;});
    assert.equal(app.service_id,apps.public.service_id);assert.equal(app.public_url,apps.public.public_url);
    assert.equal((await appInfo(apps.private.app_id)).status,'stopped');
    await wait('restored App route',async()=> (await api('/',undefined,{raw:true,headers:{Cookie:'',Host:new URL(app.public_url).host}})).status===200);
    const restored=await probe(agents.b,'proxy-cold-restored','http','http://renamed-http.treer/identity');
    assert.equal(restored.response.claims.sub,agents.b);
    const restoredApp=(await probe(agents.a,'app-proxy-cold-restored','http',`http://${app.service_id}.treer/proxy?service=${services.http}`)).response;
    assert.equal(restoredApp.claims.sub,restoredApp.runtime_id);assert.equal(restoredApp.claims.aud,services.http);
    await command(agents.b,'restored-forward',['service','forward',services.http,'--listen','127.0.0.1:8400'],{background:true});
    assert((await wait('restored private service',()=>docker('exec',names('machine-b'),'curl','--max-time','3','-fsS','http://127.0.0.1:8400/asset'))).includes('treer-e2e-a:/asset'));
    const count=await docker('exec',names('machine-b'),'python3','-c',"import subprocess;print(sum(l.strip()=='python3 /fixtures/app.py 8500' for l in subprocess.check_output(['ps','-eo','args'],text=True).splitlines()))");assert.equal(count,'1');
    const report=await cli('a','runtime','restore','--all');
    assert(report.items.some(i=>i.workload_id===recovery.invalid&&i.state==='failed'));
    for (const key of ['stopped','completed']) {
      const agent=(await snapshot()).agents.find(a=>a.agent_id===recovery[key]);
      assert(!agent||['exited','failed','stopped'].includes(agent.status),JSON.stringify(agent));
    }
  });
  await check('App delete removes service/ingress; custom hostname rejected',async()=>{
    const rejected=await api(`/api/workspaces/${ws}/apps`,{server_id:apps.public.server_id,name:'reject-custom',command:'python3',port:8504,hostname:'custom.test'},{raw:true});
    assert.equal(rejected.status,400);assert.equal(rejected.data.error.code,'platform_domain_required');
    await command(agents.a,'delete-private',['app','delete',apps.private.app_id]);
    assert.equal((await api(`/api/workspaces/${ws}/apps/${apps.private.app_id}`,undefined,{raw:true})).status,404);
    assert(!(await api(`/api/workspaces/${ws}/services`)).data.services.some(s=>s.service_id===apps.private.service_id));
  });
  console.log(`All ${results.length} checks passed. Evidence: ${dir}`);
} finally {
  // Leave stopped containers and logs for inspection; never touch other stacks.
  for (const name of containers.reverse()) {
    try { const logs=await exec('docker',['logs',name],{maxBuffer:8*1024*1024});fs.writeFileSync(path.join(dir,name+'.log'),logs.stdout+logs.stderr); } catch {}
    try { await docker('stop','-t','2',name); } catch {}
  }
  fs.writeFileSync(path.join(dir,'resources.json'),JSON.stringify({prefix,containers,network:networkCreated?names('net'):null,image},null,2));
}
