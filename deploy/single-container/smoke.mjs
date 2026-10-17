// Local/CI only. All resources and credentials are disposable, no live Treer CLI.
// node deploy/single-container/smoke.mjs <exact-SHA image already built locally>
import assert from 'node:assert/strict';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {randomBytes} from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
const exec = promisify(execFile);
const image = process.argv[2];
if (!image || process.argv.length !== 3) throw new Error('usage: smoke.mjs LOCAL_IMAGE');
const prefix = `treer-single-${randomBytes(5).toString('hex')}`;
const proxyHost = `${prefix}.treer.test:8080`;
const appHost = `console-${prefix}.treer.test:8080`;
const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
const containers = [], volumes = [];
let network = false, port, cookie = '';
// Do not forward Agent, harness, Treer, cloud or HTTP proxy credentials to Docker.
const dockerEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) =>
  ['PATH', 'HOME', 'DOCKER_CONFIG', 'DOCKER_HOST', 'DOCKER_CONTEXT', 'TMPDIR'].includes(k)));
const docker = async (...args) => {
  try {
    const result=await exec('docker', args, {env:dockerEnv, timeout:180000, maxBuffer:8*1024*1024});
    return (result.stdout+(args[0]==='logs'?result.stderr:'')).trim();
  }
  catch { throw new Error(`isolated Docker ${args[0]} failed; credential-bearing output suppressed`); }
};
async function wait(label, fn) {
  const deadline = Date.now()+120000;
  while (Date.now()<deadline) {
    try { const value = await fn(); if (value) return value; } catch {}
    await new Promise(resolve=>setTimeout(resolve,300));
  }
  throw new Error(`timeout: ${label}`);
}
async function request(route, body, {host=proxyHost, session=cookie, raw=false}={}) {
  const method = body===undefined?'GET':'POST';
  const headers = {Host:host, Cookie:session};
  const payload = body===undefined?undefined:JSON.stringify(body);
  if (payload!==undefined) Object.assign(headers, {'Content-Type':'application/json', Origin:`http://${appHost}`});
  const response = await new Promise((resolve,reject)=>{
    const req=http.request({hostname:'127.0.0.1',port,path:route,method,headers},res=>{
      const chunks=[];
      res.on('data',c=>chunks.push(c));
      res.on('end',()=>resolve({status:res.statusCode, headers:res.headers,text:Buffer.concat(chunks).toString()}));
    });
    req.setTimeout(30000,()=>req.destroy(new Error('HTTP timeout')));
    req.on('error',reject);req.end(payload);
  });
  try {response.data=JSON.parse(response.text);} catch {}
  if (!raw && (response.status<200||response.status>=300)) throw new Error(`${route} returned ${response.status}`);
  return response;
}
const takeCookie = response => {
  const cookies=response.headers['set-cookie'];
  assert(cookies?.length);
  assert(cookies.every(value=>!value.toLowerCase().includes('domain=')));
  return cookies[0].split(';')[0];
};
async function run(name, ...args) {
  await docker('run','-d','--name',name,'--network',prefix,...args);
  containers.push(name);
}
const serverOptions=volume=>[
  '--read-only','--tmpfs','/tmp:rw,nosuid,nodev','--cap-drop=ALL','--cap-add=CHOWN',
  '--cap-add=DAC_OVERRIDE','--cap-add=SETUID','--cap-add=SETGID','--cap-add=KILL',
  '--security-opt=no-new-privileges:true','--mount',`type=volume,src=${volume},dst=/data`,
];
async function fingerprints(server) {
  // Ephemeral test containers only. Hash inside the container: raw key/password
  // bytes never appear in this process's output, Docker logs or assertions.
  const key=await docker('exec','-u','postgres',server,'python3','-I','-c',
    "import subprocess,hashlib; value=subprocess.check_output(['psql','-h','/data/socket','-U','postgres','-d','treer','-Atc',\"SELECT encode(value,'hex') FROM proxy_secrets WHERE name='workload_identity_ed25519_v1'\"]); key=bytes.fromhex(value.decode().strip()); assert len(key)==32; print(hashlib.sha256(key).hexdigest())");
  const database=await docker('exec',server,'python3','-I','-c',
    "import hashlib; from pathlib import Path; print(hashlib.sha256(Path('/data/private/database-password').read_bytes()).hexdigest())");
  assert.match(key,/^[0-9a-f]{64}$/);assert.match(database,/^[0-9a-f]{64}$/);
  return {key,database};
}
async function duplicateHostStatus() {
  return await new Promise((resolve,reject)=>{
    const socket=net.connect({host:'127.0.0.1',port});
    let response='';
    socket.setTimeout(5000,()=>socket.destroy(new Error('duplicate-Host probe timeout')));
    socket.on('connect',()=>socket.write(`GET /api/health HTTP/1.1\r\nHost: ${proxyHost}\r\nHost: ${appHost}\r\nConnection: close\r\n\r\n`));
    socket.on('data',chunk=>{response+=chunk; if(response.length>65536)socket.destroy(new Error('oversized probe response'));});
    socket.on('error',reject);
    socket.on('end',()=>resolve(Number(response.match(/^HTTP\/1\.[01] (\d{3})/)?.[1])));
  });
}
try {
  await docker('image','inspect',image);
  await docker('network','create',prefix);network=true;
  const volume=`${prefix}-data`;await docker('volume','create',volume);volumes.push(volume);
  const server=`${prefix}-server`;
  await run(server,'--network-alias',proxyHost.split(':')[0],'--network-alias',appHost.split(':')[0],
    ...serverOptions(volume),
    '-p','127.0.0.1::8080','-e','ADMIN_PASSWORD=single-smoke-disposable-admin',
    '-e',`TREER_PROXY_PUBLIC_URL=http://${proxyHost}/`,'-e',`TREER_APP_PUBLIC_URL=http://${appHost}/`,image);
  port=Number((await docker('port',server,'8080/tcp')).split(':').at(-1));
  await wait('Proxy health',async()=> (await request('/api/health')).status===200);
  await docker('exec','-u','treer',server,'python3','-I','-c',
    "import os,stat; from pathlib import Path; root=Path('/app'); checksums=list(root.glob('dist/*/SHA256SUMS')); assert len(checksums)==2; [p.read_bytes() for p in checksums]; entries=[root,root/'bin',root/'dist',root/'build.json',root/'bin/treer-proxy',*root.glob('dist/*'),*root.glob('dist/*/*')]; assert all(p.stat().st_uid==0 and not p.stat().st_mode & 0o022 for p in entries); target=root/'dist/.non-root-write-probe';\ntry:\n fd=os.open(target,os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600)\nexcept PermissionError: pass\nexcept OSError as error:\n assert error.errno==30\nelse:\n os.close(fd); target.unlink(); raise AssertionError('Proxy can modify public machine artifacts')");
  assert.match((await request('/',undefined,{host:appHost,session:''})).text, /<html/i);
  const config=await request('/config.json',undefined,{host:appHost,session:''});
  assert.equal(config.data.proxy_url,`http://${proxyHost}/`);
  assert.match(config.headers['cache-control'],/no-store/);
  assert.match(config.headers['content-security-policy'],/frame-ancestors 'none'/);
  for (const [host,route,status] of [[appHost,'/api/health',404],[proxyHost,'/',404],['evil.test:8080','/',421]]) {
    const response=await request(route,undefined,{host,raw:true});assert.equal(response.status,status);
    assert(!response.text.includes('<html'));
  }
  assert.equal(await duplicateHostStatus(),400);
  const primaryFingerprints=await fingerprints(server);
  // A second fresh server deliberately chooses the degraded single-origin mode.
  const auxiliary=`${prefix}-same-origin`, auxiliaryHost=`same-${prefix}.treer.test:8080`;
  const auxiliaryVolume=`${prefix}-same-origin-data`;
  await docker('volume','create',auxiliaryVolume);volumes.push(auxiliaryVolume);
  await run(auxiliary,...serverOptions(auxiliaryVolume),'-p','127.0.0.1::8080',
    '-e',`TREER_PROXY_PUBLIC_URL=http://${auxiliaryHost}/`,'-e',`TREER_APP_PUBLIC_URL=http://${auxiliaryHost}/`,image);
  const primaryPort=port;
  port=Number((await docker('port',auxiliary,'8080/tcp')).split(':').at(-1));
  try {
    await wait('same-origin server health',async()=> (await request('/api/health',undefined,{host:auxiliaryHost,session:''})).status===200);
    const page=await request('/',undefined,{host:auxiliaryHost,session:''});
    assert.match(page.headers['content-security-policy'],/frame-ancestors 'none'/);
    for (const route of ['/api/workspaces/test/agents/test/interface/ui/', '/api/workspaces/test/virtual-hosts/test/proxy/']) {
      const response=await request(route,undefined,{host:auxiliaryHost,session:'',raw:true});
      assert.equal(response.status,503);assert.match(response.text,/embedded_ui_requires_separate_app_origin/);
    }
    const secondary=await fingerprints(auxiliary);
    assert.notEqual(secondary.key,primaryFingerprints.key);
    assert.notEqual(secondary.database,primaryFingerprints.database);
    assert.match(await docker('logs',auxiliary),/app_served_on_proxy_origin/);
  }finally{port=primaryPort;}
  const admin=takeCookie(await request('/api/admin/login',{password:'single-smoke-disposable-admin'},{session:''}));
  const invite=(await request('/api/admin/invitations',{}, {session:admin})).data.token;
  cookie=takeCookie(await request('/api/auth/register',{
    invite,email:'single-smoke@treer.invalid',preferred_name:'Single container smoke',password:'single-smoke-disposable-user'
  },{session:''}));
  const organization=(await request('/api/organizations')).data.organizations[0].organization_id;
  const workspace=(await request('/api/workspaces',{organization_id:organization,name:'Disposable smoke'})).data.workspace.workspace_id;
  // Build only a tiny disposable enrolled machine fixture, not another server.
  fs.writeFileSync(path.join(dir,'Dockerfile'),`FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl python3 procps && rm -rf /var/lib/apt/lists/* && useradd --create-home --shell /bin/sh smoke
USER smoke
ENV PATH=/home/smoke/.local/bin:/usr/local/bin:/usr/bin:/bin
WORKDIR /home/smoke
CMD ["sleep","infinity"]
`);
  const fixture=`${prefix}-fixture`;
  await docker('build','-t',fixture,dir);
  const machine=`${prefix}-machine`;
  try {
    await run(machine,'--init',fixture);
    await docker('exec',machine,'sh','-c',`curl -fsSL http://${proxyHost}/install.sh | sh`);
    const enrollment=(await request(`/api/workspaces/${workspace}/bootstrap`,{})).data.enrollment_key;
    await docker('exec',machine,'treer-agent-server','connect','--key',enrollment,'--proxy',`http://${proxyHost}/`,
      '--non-interactive','--accept-risk','--name','single-smoke');
    const cli=async(...args)=>JSON.parse(await docker('exec',machine,'treer','--workspace',workspace,...args));
    await wait('machine online',async()=> (await cli('status')).servers.some(s=>s.name==='single-smoke'&&s.status==='online'));
    const created=await cli('agent','admin','create','--machine','single-smoke','--kind','command','--name','smoke-worker','--',
      'python3','-u','-c',"import time; print('SINGLE_SMOKE_READY',flush=True); time.sleep(3600)");
    const agent=(created.agent??created).agent_id;
    await wait('agent output',async()=> (await request(`/api/workspaces/${workspace}/agents/${agent}/output`)).data.text.includes('SINGLE_SMOKE_READY'));
    await docker('stop','-t','65',server);await docker('start',server);
    // Docker reallocates an ephemeral published host port on stop/start.
    // Read the new mapping before probing persistence, rather than timing out
    // against the old socket and misreporting a lost workspace.
    port=Number((await docker('port',server,'8080/tcp')).split(':').at(-1));
    await wait('Proxy health after restart',async()=> (await request('/api/health')).status===200);
    await wait('persistent workspace after restart',async()=> (await request(`/api/workspaces/${workspace}/snapshot`)).data.agents.some(a=>a.agent_id===agent));
    assert.deepEqual(await fingerprints(server),primaryFingerprints);
    console.log('PASS single container: Host/origin/same-origin refusal/fresh keys → auth → enroll disposable machine → create Agent → persistent restart');
  } finally {
    // Remove machine before fixture image; all names were generated by this run.
    if (containers.includes(machine)) {await docker('rm','-f','-v',machine);containers.splice(containers.indexOf(machine),1);}
    await docker('image','rm',fixture);
  }
} finally {
  for (const name of containers.reverse()) await docker('rm','-f','-v',name);
  for (const name of volumes.reverse()) await docker('volume','rm',name);
  if (network) await docker('network','rm',prefix);
  fs.rmSync(dir,{recursive:true,force:true});
}
