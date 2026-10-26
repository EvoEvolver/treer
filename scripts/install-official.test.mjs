import assert from 'node:assert/strict';
import {createHash, generateKeyPairSync, sign} from 'node:crypto';
import {mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync, mkdirSync, existsSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {spawn} from 'node:child_process';
import {test} from 'node:test';
const code=readFileSync(new URL('../crates/treer-proxy/src/install-official.mjs',import.meta.url),'utf8');
const shell=readFileSync(new URL('../crates/treer-proxy/src/install-official.sh',import.meta.url),'utf8');
const key=generateKeyPairSync('ed25519');const pem=key.publicKey.export({type:'spki',format:'pem'});const digest=b=>createHash('sha256').update(b).digest('hex');const fp=digest(key.publicKey.export({type:'spki',format:'der'}));
const rendered=code.replace('__TREER_OFFICIAL_RELEASE_PEM__',pem).replace('634761ff47a6a113b4b8925f0db0e030d8bfce07f2fa228fdc0a4904874dc41f',fp);
const {downloadOfficial}=await import('data:text/javascript;base64,'+Buffer.from(rendered).toString('base64'));
const names=['treer','treer-agent-host','treer-agent-server'];
async function fixture(t,platform='linux-x86_64',mutate=()=>{}){
 const dir=mkdtempSync(join(tmpdir(),'treer-signed-install-'));t.after(()=>rmSync(dir,{recursive:true,force:true}));const objects=new Map(),requests=[];
 const manifest={schema_version:2,version:'v0.1.16',git_commit:'f'.repeat(40),signing_key_id:fp.slice(0,16),artifacts:[]};
 for(const name of names){const path=`releases/v0.1.16/${platform}/${name}`;const b=Buffer.from(`#!/bin/sh\ntouch should-never-execute\n#${name}`);objects.set(path,b);manifest.artifacts.push({binary:name,platform,path,size:b.length,sha256:digest(b)});}
 mutate({manifest,objects});
 const putSigned=(path,value)=>{const b=Buffer.from(JSON.stringify(value));objects.set(path,b);objects.set(path+'.sig',Buffer.from(JSON.stringify({schema_version:1,algorithm:'ed25519',key_id:fp.slice(0,16),signature:sign(null,b,key.privateKey).toString('base64')})));return b;};
 const m=putSigned('releases/v0.1.16/manifest.json',manifest);putSigned('channels/stable.json',{schema_version:1,channel:'stable',version:'v0.1.16',manifest_path:'releases/v0.1.16/manifest.json',manifest_sha256:digest(m)});
 const transport=async (url,options)=>{assert.equal(url.origin,'https://releases.treer.ai');assert.equal(options.redirect,'error');assert.ok(options.signal);const path=url.pathname.slice(1);requests.push(path);return new Response(objects.get(path),{status:objects.has(path)?200:404});};
 return {dir,objects,requests,transport};
}
for(const p of ['linux-x86_64','linux-aarch64','darwin-x86_64','darwin-aarch64'])test(`signed stable stages exact ${p} without execution`,async t=>{const f=await fixture(t,p);const r=await downloadOfficial(p,f.dir,{transport:f.transport});assert.equal(r.version,'v0.1.16');assert.deepEqual(readdirSync(f.dir).sort(),[...names].sort());assert.equal(f.requests.length,7);for(const n of names)assert.match(readFileSync(join(f.dir,n),'utf8'),/should-never-execute/);});
for(const where of ['channels/stable.json','releases/v0.1.16/manifest.json'])test(`tampered ${where} refuses before binaries`,async t=>{const f=await fixture(t);f.objects.set(where,Buffer.from('{}'));await assert.rejects(downloadOfficial('linux-x86_64',f.dir,{transport:f.transport}));assert.deepEqual(readdirSync(f.dir),[]);});
for(const [name,mutate] of [
 ['duplicate artifact',({manifest:m})=>m.artifacts.push(m.artifacts[0])],
 ['traversal',({manifest:m})=>m.artifacts[0].path='../treer'],
 ['foreign origin',({manifest:m})=>m.artifacts[0].path='https://evil.invalid/treer'],
 ['oversized artifact',({manifest:m})=>m.artifacts[0].size=129*1024*1024],
 ['wrong platform',({manifest:m})=>m.artifacts[0].platform='windows-x86_64'],
 ['bad artifact hash',({manifest:m})=>m.artifacts[0].sha256='0'.repeat(64)]
])test(`${name} refuses`,async t=>{const f=await fixture(t,'linux-x86_64',mutate);await assert.rejects(downloadOfficial('linux-x86_64',f.dir,{transport:f.transport}));assert.deepEqual(readdirSync(f.dir),[]);});
test('redirect refusal and bounded metadata',async t=>{const f=await fixture(t);for(const response of [()=>new Response('',{status:302}),()=>new Response('x'.repeat(1024*1024+1))])await assert.rejects(downloadOfficial('linux-x86_64',f.dir,{transport:async()=>response()}));assert.deepEqual(readdirSync(f.dir),[]);});
test('generated shell keeps installed files unchanged when signed download fails',async t=>{const f=await fixture(t);const home=join(f.dir,'home');mkdirSync(home);const bins=join(home,'bin');mkdirSync(bins);writeFileSync(join(bins,'treer'),'original');
 const js=rendered.replace("if (process.argv[1] === '-') {","if (process.argv[1] === '-') {\n globalThis.fetch = async () => new Response('{}');");
 const script=shell.replace('__TREER_OFFICIAL_DOWNLOAD_JS__',js).replace('__TREER_PATH_SETUP__','');
 const result=await new Promise((resolve,reject)=>{const p=spawn('sh',['-eu'],{env:{...process.env,HOME:home,TMPDIR:home,TREER_INSTALL_DIR:bins,TREER_AGENT_SERVER_INSTALL_DIR:join(home,'server')},stdio:['pipe','ignore','pipe']});let stderr='';p.stderr.on('data',b=>stderr+=b);p.on('error',reject);p.on('exit',status=>resolve({status,stderr}));p.stdin.end(script);});
 assert.notEqual(result.status,0);assert.match(result.stderr,/official signed bootstrap refused/);assert.equal(readFileSync(join(bins,'treer'),'utf8'),'original');assert.equal(existsSync(join(home,'server')),false);assert.deepEqual(readdirSync(home).sort(),['bin']);});
test('production source has fixed canonical authority and no environment trust override',()=>{const pem=readFileSync(new URL('../keys/official-release-ed25519.pub.pem',import.meta.url),'utf8');assert.equal(digest(generatePublic(pem)),'634761ff47a6a113b4b8925f0db0e030d8bfce07f2fa228fdc0a4904874dc41f');assert.doesNotMatch(code,/process\.env/);});
import {createPublicKey} from 'node:crypto';function generatePublic(pem){return createPublicKey(pem).export({type:'spki',format:'der'});}

test('rendered signed shell installs only the verified set into isolated paths, without executing it',async t=>{
 const platform=process.platform==='darwin'?'darwin-':'linux-';const f=await fixture(t,platform+(process.arch==='arm64'?'aarch64':'x86_64'));const home=join(f.dir,'home');mkdirSync(home);const bins=join(home,"bin with 'quotes'");const servers=join(home,'libexec');
 const data=JSON.stringify([...f.objects].map(([path,b])=>[path,b.toString('base64')]));
 const bridge=`globalThis.fetch = async (url) => {const objects=new Map(${data});const b=objects.get(new URL(url).pathname.slice(1));return new Response(b?Buffer.from(b,'base64'):null,{status:b?200:404});};`;
 const js=rendered.replace("if (process.argv[1] === '-') {", "if (process.argv[1] === '-') {\n"+bridge);
 const script=shell.replace('__TREER_OFFICIAL_DOWNLOAD_JS__',js).replace('__TREER_PATH_SETUP__','');
 const p=spawn('sh',['-eu'],{env:{...process.env,HOME:home,TMPDIR:home,TREER_INSTALL_DIR:bins,TREER_AGENT_SERVER_INSTALL_DIR:servers},stdio:['pipe','pipe','pipe']});let stderr='',stdout='';p.stderr.on('data',b=>stderr+=b);p.stdout.on('data',b=>stdout+=b);const status=new Promise((resolve,reject)=>{p.on('exit',resolve);p.on('error',reject);});p.stdin.end(script);assert.equal(await status,0,stderr);assert.match(stdout,/verified official stable v0.1.16/);
 for(const name of names)assert.match(readFileSync(join(name==='treer'?bins:servers,name),'utf8'),/should-never-execute/);
 assert.equal(existsSync(join(home,'should-never-execute')),false);
});

test('all Proxy Docker builders copy only the canonical public compile-time key',()=>{
 const input='keys/official-release-ed25519.pub.pem';
 for(const name of ['Dockerfile','Dockerfile.hosted','Dockerfile.single']){
  const text=readFileSync(new URL('../'+name,import.meta.url),'utf8');
  assert.match(text,/COPY keys\/official-release-ed25519\.pub\.pem \.\/keys\/official-release-ed25519\.pub\.pem/);
  assert.doesNotMatch(text,/^COPY keys(?:\s|\/\s)/m,'never copy an entire custody directory');
  assert.ok(text.indexOf('COPY '+input)<text.indexOf('cargo build'),'key must reach the Rust builder before compilation');
 }
 assert.equal(readFileSync(new URL('../'+input,import.meta.url),'utf8').includes('PRIVATE KEY'),false);
 assert.doesNotMatch(readFileSync(new URL('../.dockerignore',import.meta.url),'utf8'),/^(?:keys|keys\/|\*\.pem)$/m);
});
