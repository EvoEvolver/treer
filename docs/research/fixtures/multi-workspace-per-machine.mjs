// macOS isolated reproduction. Run from a private worktree after building
// treer-proxy, treer-cli, treer-agent-server and treer-agent-host. Requires
// Docker, Node 22+, npm beside Node, and /usr/bin/python3. Synthetic identities
// only; all service commands use fixture HOME and explicit fixture workspace.
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import net from 'node:net';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
const repo=process.cwd(), fixtureBase=path.join(repo,'.references/multi-fixtures');
fs.mkdirSync(fixtureBase,{recursive:true,mode:0o700});
const run=fs.realpathSync(fs.mkdtempSync(path.join(fixtureBase,'mw-'))), bin=path.join(run,'bin');
fs.mkdirSync(bin,{mode:0o700});
for(const name of ['treer','treer-agent-server','treer-agent-host','treer-proxy']) fs.copyFileSync(path.join(repo,'target/debug',name),path.join(bin,name));
const home=path.join(run,'home'), runtime=fs.realpathSync(fs.mkdtempSync('/tmp/trr-mws-'));
for(const dir of [home,path.join(run,'root-a'),path.join(run,'root-b')]) fs.mkdirSync(dir,{mode:0o700});
const env={HOME:home,USER:os.userInfo().username,LOGNAME:os.userInfo().username,PATH:`${bin}:/usr/bin:/bin:/usr/sbin:/sbin`,LANG:'en_US.UTF-8',XDG_CONFIG_HOME:path.join(home,'.config'),XDG_STATE_HOME:path.join(home,'.state'),XDG_CACHE_HOME:path.join(home,'.cache'),XDG_DATA_HOME:path.join(home,'.data'),TREER_RUNTIME_DIR:runtime,TMPDIR:run};
const results=[], containers=[], processes=[], workspaces=[];
let cookie='',base;
fs.mkdirSync(path.join(repo,'.treer'),{recursive:true,mode:0o700});
const evidence=path.join(repo,'.treer/multi-workspace-evidence.json');
function record(label,data={}){results.push({label,...data});fs.writeFileSync(evidence,JSON.stringify({source:'multi-workspace-fixes',run,results},null,2),{mode:0o600});console.log(label,JSON.stringify(data));}
function docker(args){return execFileSync('docker',args,{encoding:'utf8',timeout:90000}).trim();}
function child(name,args,extra={}){return new Promise(resolve=>{const p=spawn(path.join(bin,name),args,{env:{...env,...extra},cwd:run,stdio:['ignore','pipe','pipe']});let out='',err='';p.stdout.on('data',x=>out+=x);p.stderr.on('data',x=>err+=x);const t=setTimeout(()=>p.kill('SIGKILL'),90000);p.on('close',code=>{clearTimeout(t);resolve({code,out,err});});});}
function requireOK(result,label){if(result.code!==0) throw new Error(`${label} failed exit ${result.code}: ${result.err.split('\n')[0].replaceAll(run,'[fixture]')}`);return result.out;}
async function cli(ws,...args){const r=await child('treer',['--workspace',ws,...args]);return JSON.parse(requireOK(r,args[0]));}
async function service(ws,action){return child('treer-agent-server',['service','--workspace',ws,action]);}
async function api(route,body,headers={}){const res=await fetch(base+route,{method:body===undefined?'GET':'POST',headers:{Cookie:cookie,'Content-Type':'application/json',...headers},body:body===undefined?undefined:JSON.stringify(body)});const text=await res.text();let data;try{data=JSON.parse(text);}catch{data={};}if(!res.ok)throw new Error(`API ${route.split('?')[0]} failed status ${res.status}`);return {data,headers:res.headers};}
async function wait(fn,limit=20000){const stop=Date.now()+limit;while(Date.now()<stop){try{const r=await fn();if(r)return r;}catch{}await new Promise(r=>setTimeout(r,100));}throw new Error('fixture wait timeout');}
async function port(){const s=net.createServer();await new Promise(r=>s.listen(0,'127.0.0.1',r));const p=s.address().port;await new Promise(r=>s.close(r));return p;}
function config(ws){const dir=path.join(home,'.config/treer/agent-servers');for(const name of fs.readdirSync(dir)){if(!name.endsWith('-controller.json'))continue;const value=JSON.parse(fs.readFileSync(path.join(dir,name)));if(value.workspace===ws)return {...value,file:path.join(dir,name)};}throw new Error('no fixture config');}
async function snapshot(ws){return (await api(`/api/workspaces/${ws}/snapshot`)).data;}
async function health(ws){const c=config(ws);const r=await fetch(`http://${c.listen}/api/health`);return r.json();}
function pid(ws){const c=config(ws);return JSON.parse(fs.readFileSync(path.join(home,'.state/treer',`agent-host-${c.server_id}.pid`))).pid;}
try{
 const prefix=`trr-mw-${crypto.randomBytes(4).toString('hex')}`;
 for(const [suffix,image,args] of [['pg','postgres:17-alpine',['-e','POSTGRES_PASSWORD=fixture-only','-e','POSTGRES_USER=treer','-e','POSTGRES_DB=treer','-p','127.0.0.1::5432']],['nats','nats:2.11-alpine',['-p','127.0.0.1::4222']]]){const name=prefix+'-'+suffix;docker(['run','-d','--name',name,...args,image,...(suffix==='nats'?['-js']:[])]);containers.push(name);}
 await wait(()=>docker(['exec',containers[0],'pg_isready','-U','treer']).includes('accepting'));
 const pg=docker(['port',containers[0],'5432/tcp']),nats=docker(['port',containers[1],'4222/tcp']);base=`http://127.0.0.1:${await port()}`;
 const log=fs.openSync(path.join(run,'proxy.log'),'w',0o600);
 const proxy=spawn(path.join(bin,'treer-proxy'),['--listen',new URL(base).host,'--public-url',base,'--app-public-url',base],{env:{...env,DATABASE_URL:`postgres://treer:fixture-only@${pg}/treer`,TREER_NATS_URL:`nats://${nats}`,ADMIN_PASSWORD:'fixture-admin'},cwd:run,stdio:['ignore',log,log]});processes.push(proxy);fs.closeSync(log);
 await wait(async()=> (await fetch(base+'/api/health')).ok);
 const admin=await api('/api/admin/login',{password:'fixture-admin'});const adminCookie=admin.headers.get('set-cookie').split(';')[0];
 const invite=(await api('/api/admin/invitations',{}, {Cookie:adminCookie})).data.token;
 const user=await api('/api/auth/register',{invite,email:'multi-workspace@fixture.invalid',preferred_name:'Multi workspace fixture',password:'fixture-user-password'});cookie=user.headers.get('set-cookie').split(';')[0];
 const org=(await api('/api/organizations')).data.organizations[0].organization_id;
 for(const side of ['a','b']){const ws=(await api('/api/workspaces',{organization_id:org,name:`fixture-${side}`})).data.workspace.workspace_id;workspaces.push(ws);const key=(await api(`/api/workspaces/${ws}/bootstrap`,{})).data.enrollment_key;
   const connected=await child('treer-agent-server',['connect','--key',key,'--proxy',base,'--root',path.join(run,'root-'+side),'--service-mode','nohup','--non-interactive','--accept-risk','--name','same-physical-host']);requireOK(connected,'connect '+side);if(side==='b'){assert(connected.err.includes('One OS account is one trust domain'));record('second-enrollment-warns-shared-account');}
   await wait(async()=> (await snapshot(ws)).servers.some(s=>s.status==='online'));}
 const [a,b]=workspaces,ca=config(a),cb=config(b),ha=await health(a),hb=await health(b);
 assert.notEqual(ca.server_id,cb.server_id);assert.notEqual(ca.listen,cb.listen);assert.notEqual(pid(a),pid(b));assert.notEqual(ha.controller_epoch,hb.controller_epoch);
 assert((fs.statSync(ca.file).mode&0o777)===0o600);assert((fs.statSync(cb.file).mode&0o777)===0o600);
 record('two-nohup-enrollments',{ports:[ca.listen,cb.listen],different_server_ids:true,different_host_pids:true,different_controller_epochs:true,configuration_mode:'0600'});
 const ambiguous=await child('treer',['status']);assert.notEqual(ambiguous.code,0);assert(ambiguous.err.includes('multiple installed Controllers'));record('CLI-no-workspace-fails-closed');
 const ambiguousService=await child('treer-agent-server',['service','stop']);assert.notEqual(ambiguousService.code,0);record('service-stop-without-selection-fails-closed');
 const agents=[],apps=[];
 for(const [index,ws] of workspaces.entries()){
   const c=config(ws), status=await cli(ws,'status');assert.equal(status.servers.length,1);assert.equal(status.servers[0].server_id,c.server_id);
   const exec=await cli(ws,'exec','--machine',c.server_id,'--','/usr/bin/printf','fixture');assert.equal(exec.stdout,'fixture');
   const probe=`import os,json,time,urllib.parse;print(json.dumps({'correct_workspace':os.environ.get('TREER_WORKSPACE_ID')==${JSON.stringify(ws)},'has_identity':bool(os.environ.get('TREER_AGENT_ID')) and bool(os.environ.get('TREER_WORKLOAD_CREDENTIAL')),'proxy_port':urllib.parse.urlparse(os.environ.get('TREER_HTTP_PROXY','')).port}),flush=True);time.sleep(300)`;
   const agentResult=await cli(ws,'agent','admin','create','--machine',c.server_id,'--kind','command','--name','same-agent','--','/usr/bin/python3','-u','-c',probe);const agent=agentResult.agent??agentResult;agents.push(agent.agent_id);
   const startup=await cli(ws,'agent','admin','startup','set',agent.agent_id,'--cwd','.','--','/bin/sh','-c','exec /bin/sleep 300');assert(startup);
   const local=path.join(run,`upload-${index}`),dest=path.join(run,`download-${index}`);fs.writeFileSync(local,`fixture-${index}`);
   requireOK(await child('treer',['--workspace',ws,'machine','put','--machine',c.server_id,local,'roundtrip.txt']),'put');
   requireOK(await child('treer',['--workspace',ws,'machine','get','--machine',c.server_id,'roundtrip.txt',dest]),'get');assert.equal(fs.readFileSync(local,'utf8'),fs.readFileSync(dest,'utf8'));
   const python='/usr/bin/python3',appPort=await port();
   const appResult=await cli(ws,'app','create','--machine',c.server_id,'--name','same-app','--cwd','.','--port',String(appPort),python,'--','-m','http.server',String(appPort),'--bind','127.0.0.1');const app=appResult.app??appResult;apps.push(app.service_id);
   await wait(async()=> (await fetch(`http://127.0.0.1:${appPort}/roundtrip.txt`)).ok);
   record('workspace-'+index+'-operations',{exec:true,agent_create:true,put_get:true,app_create:true});
   const output=await wait(async()=> {const t=(await api(`/api/workspaces/${ws}/agents/${agent.agent_id}/output`)).data.text;const match=t.match(/\{"correct_workspace"[^\n]+/);return match&&JSON.parse(match[0]);});assert(output.correct_workspace);assert(output.has_identity);assert(output.proxy_port);record('agent-environment-'+index,output);
 }
 const cross=await child('treer',['--workspace',a,'agent','show',agents[1]]);assert.notEqual(cross.code,0);record('cross-workspace-target-id-rejected');
 // Workload credentials are accessed only inside this synthetic fixture, never printed.
 const sa=JSON.parse(fs.readFileSync(path.join(home,'.state/treer',`${ca.server_id}-agent-startup.json`)));
 const findCredential=x=>{if(x&&typeof x==='object'){if(x.workload_credential)return x.workload_credential;for(const value of Object.values(x)){const found=findCredential(value);if(found)return found;}}};
 const credential=findCredential(sa);assert(credential);
 const bound={TREER_WORKSPACE_ID:a,TREER_AGENT_SERVER_URL:`http://${ca.listen}`,TREER_AGENT_ID:agents[0],TREER_WORKLOAD_CREDENTIAL:credential,TREER_IDENTITY_MODE:'agent'};
 const operatorBound={TREER_WORKSPACE_ID:a,TREER_AGENT_SERVER_URL:`http://${ca.listen}`,TREER_OPERATOR_CREDENTIAL:ca.operator_credential};
 for(const [label,identity] of [['agent',bound],['operator-exec',operatorBound]]){
   for(const flags of [['--workspace',b],['--url',`http://${cb.listen}`]]){const result=await child('treer',[...flags,'status'],identity);assert.notEqual(result.code,0);assert(result.err.includes(`current identity belongs to workspace ${a}`));assert(result.err.includes('operator terminal'));assert(!result.err.includes(credential));assert(!result.err.includes(ca.operator_credential));}
   for(const flags of [[],['--workspace',a],['--url',`http://${ca.listen}`]]){const result=await child('treer',[...flags,'status'],identity);requireOK(result,'same-identity CLI');}
   record('bound-CLI-selection-'+label,{different_workspace_rejected:true,different_url_rejected:true,default_and_same_selection_work:true});
 }
 const forged=await child('treer',['status'],{TREER_WORKSPACE_ID:b,TREER_AGENT_SERVER_URL:`http://${cb.listen}`,TREER_AGENT_ID:agents[0],TREER_WORKLOAD_CREDENTIAL:credential,TREER_IDENTITY_MODE:'agent'});assert.notEqual(forged.code,0);record('foreign-workload-credential-rejected');
 const leak=await cli(a,'exec','--machine',ca.server_id,'--','/bin/test','-r',cb.file);assert.equal(leak.exit_code,0);record('same-OS-account-can-read-other-workspace-config',{readable:true,credential_values_printed:false});
 const socketProbe=await cli(a,'exec','--machine',ca.server_id,'--','/usr/bin/python3','-c',`import socket;s=socket.socket(socket.AF_UNIX);s.settimeout(2);s.connect(${JSON.stringify(cb.host_socket)});s.close()`);assert.equal(socketProbe.exit_code,0);
 const unauth=await cli(a,'exec','--machine',ca.server_id,'--','/usr/bin/python3','-c',`import urllib.request,urllib.error;\ntry: urllib.request.urlopen('http://${cb.listen}/api/local/agents',timeout=2)\nexcept urllib.error.HTTPError as e: print(e.code)`);assert.equal(unauth.exit_code,0);assert.equal(unauth.stdout.trim(),'401');record('same-account-boundary',{read_other_config:true,connect_other_host_socket:true,unauthenticated_loopback_status:401});
 const bEpoch=(await health(b)).controller_epoch,bPid=pid(b),aPid=pid(a);
 const beforeGeneration=(await snapshot(a)).agents.find(x=>x.agent_id===agents[0]).run_generation;
 requireOK(await service(a,'restart-controller'),'hot restart a');await wait(async()=> (await health(a)).controller_epoch!==ha.controller_epoch);assert.equal(pid(a),aPid);assert.equal(pid(b),bPid);assert.equal((await health(b)).controller_epoch,bEpoch);record('hot-restart-only-selected-controller');
 process.kill(aPid,'SIGKILL');await wait(async()=>!(await fetch(`http://${ca.listen}/api/health`).catch(()=>null)));
 requireOK(await child('treer-agent-server',['restore','--workspace',a]),'cold restore a');await wait(async()=> (await snapshot(a)).agents.some(x=>x.agent_id===agents[0]&&['working','idle','blocked','starting'].includes(x.status)));
 assert.equal(pid(b),bPid);assert.equal((await health(b)).controller_epoch,bEpoch);record('cold-restore-only-selected-workspace');
 assert((await snapshot(a)).agents.find(x=>x.agent_id===agents[0]).run_generation>beforeGeneration);record('cold-restore-keeps-agent-id-and-advances-generation');
 // Test shared file replacement only in the fixture bin. Same-version bytes
 // prove live inode stability, not mixed-version compatibility.
 for(const name of ['treer','treer-agent-server','treer-agent-host']){const target=path.join(bin,name),candidate=target+'.next';fs.copyFileSync(target,candidate);fs.renameSync(candidate,target);}
 await health(a);assert.equal((await health(b)).controller_epoch,bEpoch);requireOK(await service(a,'restart-controller'),'fixture shared binary replacement');record('shared-binary-atomic-same-version-replacement',{both_live:true,mixed_version_not_proven:true});
 const node=fs.realpathSync(process.execPath),npm=path.resolve(node,'../../lib/node_modules/npm/bin/npm-cli.js');
 for(const ws of workspaces){const c=config(ws);const result=await child('treer-agent-server',['launcher','--node',node,'--treer',path.join(bin,'treer'),'install','--allow-network','--npm-cli',npm],{TREER_WORKSPACE_ID:ws});
   record('launcher-install-'+workspaces.indexOf(ws),{exit:result.code,error_category:result.code===0?null:result.err.includes('unsupported')?'unsupported':result.err.includes('trusted')?'trusted-path':result.err.includes('profile')?'profile':'other'});
   let installed=result.code===0;
   if(!installed){const wrapper=path.join(bin,'treer-'+workspaces.indexOf(ws));fs.writeFileSync(wrapper,`#!/bin/sh\nexec '${path.join(bin,'treer')}' --workspace '${ws}' "$@"\n`,{mode:0o700});
     const wrapped=await child('treer-agent-server',['launcher','--node',node,'--treer',wrapper,'install','--allow-network','--npm-cli',npm],{TREER_WORKSPACE_ID:ws});installed=wrapped.code===0;record('launcher-install-with-scoped-cli-wrapper-'+workspaces.indexOf(ws),{exit:wrapped.code,error_category:wrapped.code===0?null:wrapped.err.includes('unsupported')?'unsupported':wrapped.err.includes('trusted')?'trusted-path':wrapped.err.includes('profile')?'profile':wrapped.err.includes('dependency')?'dependency':'other'});}
   if(installed){const profiles=await cli(ws,'agent','admin','profile','list');const managed=(profiles.profiles??profiles).filter(p=>(p.args??[]).some(arg=>arg.startsWith(c.root+'/.treer/launchers/agent/')));assert.equal(managed.length,6);record('launcher-profiles-'+workspaces.indexOf(ws),{count:(profiles.profiles??profiles).length,installed_six_in_own_root:managed.length===6,root_has_pointer:fs.existsSync(path.join(c.root,'.treer/launchers/agent/current.json'))});}}
 const collision=(await api('/api/workspaces',{organization_id:org,name:'port-collision'})).data.workspace.workspace_id;workspaces.push(collision);
 const collisionKey=(await api(`/api/workspaces/${collision}/bootstrap`,{})).data.enrollment_key;
 // Hold a port without a Controller, then retry the exact command/key after
 // releasing it. No claim/config/state should exist on the failed attempt.
 const occupied=net.createServer();await new Promise(r=>occupied.listen(0,'127.0.0.1',r));const collisionListen=`127.0.0.1:${occupied.address().port}`;
 const collisionArgs=['connect','--key',collisionKey,'--proxy',base,'--root',ca.root,'--listen',collisionListen,'--service-mode','nohup','--non-interactive','--accept-risk','--name','port-collision'];
 const beforeConfigNames=fs.readdirSync(path.dirname(ca.file)).sort(),beforeStateNames=fs.readdirSync(path.join(home,'.state/treer')).sort();
 const conflict=await child('treer-agent-server',collisionArgs);assert.notEqual(conflict.code,0);assert(conflict.err.includes('port is already in use'));assert.deepEqual(fs.readdirSync(path.dirname(ca.file)).sort(),beforeConfigNames);assert.deepEqual(fs.readdirSync(path.join(home,'.state/treer')).sort(),beforeStateNames);assert.equal((await health(b)).controller_epoch,bEpoch);
 assert.equal((await snapshot(collision)).servers.length,0);await new Promise(r=>occupied.close(r));
 requireOK(await child('treer-agent-server',collisionArgs),'retry same connect/key');await wait(async()=> (await snapshot(collision)).servers.some(s=>s.status==='online'));record('explicit-port-collision',{failed_without_config_or_state:true,enrollment_key_unconsumed:true,same_command_retry_succeeded:true,other_controller_unchanged:true});
 const key=(await api(`/api/workspaces/${a}/bootstrap`,{})).data.enrollment_key;
 requireOK(await child('treer-agent-server',['connect','--key',key,'--proxy',base,'--root',ca.root,'--service-mode','nohup','--non-interactive','--accept-risk','--name','same-physical-host']),'repeat connect a');assert.equal(config(a).server_id,ca.server_id);assert.equal(pid(b),bPid);assert.equal((await health(b)).controller_epoch,bEpoch);record('repeat-connect-reuses-only-selected-enrollment');
 requireOK(await service(a,'stop'),'stop a');assert.equal(pid(b),bPid);await health(b);record('stop-only-selected-workspace');
 requireOK(await service(a,'uninstall'),'uninstall a');assert(fs.existsSync(cb.file));await cli(b,'status');record('uninstall-keeps-other-workspace-and-shared-binaries');
 record('finished');
}catch(error){record('FAILED',{message:error.message});process.exitCode=1;}
finally{
 for(const ws of workspaces){await service(ws,'stop').catch(()=>{});await service(ws,'uninstall').catch(()=>{});}
 for(const p of processes){p.kill('SIGTERM');}
 for(const name of containers){try{docker(['rm','-f',name]);}catch{}}
 record('fixture-cleanup',{containers_removed:true});
}
