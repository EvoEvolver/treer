// Local/CI only. All resources and credentials are disposable, no live Treer CLI.
// node deploy/single-container/smoke.mjs <exact-SHA image already built locally>
import assert from 'node:assert/strict';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {randomBytes} from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
const exec = promisify(execFile);
const image = process.argv[2];
if (!image || process.argv.length !== 3) throw new Error('usage: smoke.mjs LOCAL_IMAGE');
const prefix = `treer-single-${randomBytes(5).toString('hex')}`;
const proxyHost = `${prefix}.test:8080`;
const appHost = `console-${prefix}.test:8080`;
const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
const containers = [], volumes = [];
let network = false, port, cookie = '';
// Do not forward Agent, harness, Treer, cloud or HTTP proxy credentials to Docker.
const dockerEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) =>
  ['PATH', 'HOME', 'DOCKER_CONFIG', 'DOCKER_HOST', 'DOCKER_CONTEXT', 'TMPDIR'].includes(k)));
const docker = async (...args) => {
  try { return (await exec('docker', args, {env:dockerEnv, timeout:180000, maxBuffer:8*1024*1024})).stdout.trim(); }
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
try {
  await docker('image','inspect',image);
  await docker('network','create',prefix);network=true;
  const volume=`${prefix}-data`;await docker('volume','create',volume);volumes.push(volume);
  const server=`${prefix}-server`;
  await run(server,'--network-alias',proxyHost.split(':')[0],'--network-alias',appHost.split(':')[0],
    '--read-only','--tmpfs','/tmp:rw,nosuid,nodev','--cap-drop=ALL','--cap-add=CHOWN',
    '--cap-add=DAC_OVERRIDE','--cap-add=SETUID','--cap-add=SETGID','--cap-add=KILL',
    '--security-opt=no-new-privileges:true','--mount',`type=volume,src=${volume},dst=/data`,
    '-p','127.0.0.1::8080','-e','ADMIN_PASSWORD=single-smoke-disposable-admin',
    '-e',`TREER_PROXY_PUBLIC_URL=http://${proxyHost}/`,'-e',`TREER_APP_PUBLIC_URL=http://${appHost}/`,image);
  port=Number((await docker('port',server,'8080/tcp')).split(':').at(-1));
  await wait('Proxy health',async()=> (await request('/api/health')).status===200);
  assert.match((await request('/',undefined,{host:appHost,session:''})).text, /<html/i);
  const config=await request('/config.json',undefined,{host:appHost,session:''});
  assert.equal(config.data.proxy_url,`http://${proxyHost}/`);
  assert.match(config.headers['cache-control'],/no-store/);
  assert.match(config.headers['content-security-policy'],/frame-ancestors 'none'/);
  for (const [host,route] of [[appHost,'/api/health'],[proxyHost,'/'],['evil.test:8080','/']]) {
    const response=await request(route,undefined,{host,raw:true});assert(response.status>=400);
    assert(!response.text.includes('<html'));
  }
  const admin=takeCookie(await request('/api/admin/login',{password:'single-smoke-disposable-admin'},{session:''}));
  const invite=(await request('/api/admin/invitations',{}, {session:admin})).data.token;
  cookie=takeCookie(await request('/api/auth/register',{
    invite,email:'single-smoke@treer.invalid',preferred_name:'Single container smoke',password:'single-smoke-disposable-user'
  },{session:''}));
  const organization=(await request('/api/organizations')).data.organizations[0].organization_id;
  const workspace=(await request('/api/workspaces',{organization_id:organization,name:'Disposable smoke'})).data.workspace.workspace_id;
  // Build only a tiny disposable enrolled machine fixture, not another server.
  fs.writeFileSync(path.join(dir,'Dockerfile'),`FROM debian:bookworm-slim
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
    await wait('persistent workspace after restart',async()=> (await request(`/api/workspaces/${workspace}/snapshot`)).data.agents.some(a=>a.agent_id===agent));
    console.log('PASS single container: static/Host/origin/auth → enroll disposable machine → create Agent → persistent restart');
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
