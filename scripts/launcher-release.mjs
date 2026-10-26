#!/usr/bin/env node
// Independent launcher publication; never switches a local default or a Core channel.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { lstatSync, readFileSync, writeFileSync, mkdirSync, realpathSync } from 'node:fs';
import { join, resolve, isAbsolute } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { verifyBundle } from './prepare-launcher-release.mjs';
const repo = 'EvoEvolver/treer';
const sha = b => createHash('sha256').update(b).digest('hex');
const fullSha = s => /^[a-f0-9]{40}$/.test(s || '');
const hash = s => /^[a-f0-9]{64}$/.test(s || '');
const platforms = new Set(['darwin-aarch64', 'darwin-x86_64', 'linux-x86_64', 'linux-aarch64', 'windows-x86_64']);
function read(path, limit) {
  const s = lstatSync(path);
  assert.ok(s.isFile() && !s.isSymbolicLink() && s.size <= limit, 'Expected bounded physical regular file');
  return readFileSync(path);
}
const encode = value => Buffer.from(JSON.stringify(value) + '\n');
const url = (tag, asset) => `https://github.com/${repo}/releases/download/${tag}/${asset}`;
export function stagePackage({ payload, native, binding, build, source, platform, output }) {
  assert.ok(fullSha(source) && platforms.has(platform));
  for (const p of [payload, native, binding, build, output]) assert.ok(isAbsolute(p));
  assert.equal(realpathSync.native(payload), payload, 'Payload root must be physical');
  verifyBundle(payload, source);
  const manifestBytes = read(join(payload, 'bundle-manifest.json'), 65536);
  const manifest = JSON.parse(manifestBytes);
  assert.ok(!manifest.files.some(f => f.path === 'native-runtime.json'), 'Machine-specific runtime manifest must not be published');
  const pins = JSON.parse(read(binding, 65536));
  const proof = JSON.parse(read(build, 65536));
  assert.equal(pins.schema, 'treer.native-launcher/v1');
  assert.ok(fullSha(pins.source_commit));
  const nodePlatform = { darwin: 'darwin', linux: 'linux', win32: 'windows' }[pins.platform];
  const cpu = { arm64: 'aarch64', x64: 'x86_64' }[pins.arch];
  assert.equal(`${nodePlatform}-${cpu}`, platform);
  assert.equal(proof.schema, 'treer.launcher-native-build/v1');
  assert.equal(proof.ordinary, true); assert.equal(proof.fixture_harness, false);
  assert.equal(proof.source_commit, pins.source_commit); assert.equal(proof.platform, platform);
  const binary = read(native, 32 * 1024 * 1024);
  assert.equal(sha(binary), pins.executable.sha256); assert.equal(binary.length, pins.executable.bytes);
  assert.equal(sha(binary), proof.sha256); assert.equal(binary.length, proof.bytes);
  const magic = binary.subarray(0, 4).toString('hex');
  assert.ok(platform.startsWith('linux-') ? magic === '7f454c46' : platform.startsWith('windows-') ? magic.startsWith('4d5a') : ['cffaedfe','feedfacf','cafebabe','bebafeca'].includes(magic), 'Native platform header required');
  const tools = pins.tools;
  assert.ok(tools?.treer && (tools.codex || tools.claude));
  for (const [name, tool] of Object.entries(tools)) {
    assert.ok(['treer','codex','claude'].includes(name));
    assert.ok(isAbsolute(tool.path) && !/[\x00-\x1f\x7f]/.test(tool.path) && hash(tool.sha256) && Number.isSafeInteger(tool.bytes) && tool.bytes > 0 && typeof tool.version === 'string' && tool.version.length > 0);
    assert.equal(proof.tool_paths[name], tool.path, 'Compiled native tool paths must equal runtime binding');
  }
  assert.deepEqual(Object.keys(proof.tool_paths).sort(), Object.keys(tools).sort());
  const files = [];
  function file(path, bytes, executable) {
    assert.ok(bytes.length <= 32 * 1024 * 1024);
    files.push({path, size:bytes.length, sha256:sha(bytes), executable, data_base64:bytes.toString('base64')});
  }
  file('payload/bundle-manifest.json', manifestBytes, false);
  for (const item of manifest.files) file(`payload/${item.path}`, read(join(payload, item.path), 32 * 1024 * 1024), item.executable);
  file('native/treer-agent', binary, true);
  assert.ok(files.length <= 1024 && files.reduce((n,f) => n+f.size,0) <= 192 * 1024 * 1024);
  const value = {schema:'treer.launcher-package/v1', source_commit:source, platform, payload_manifest_sha256:sha(manifestBytes), native:{path:'native/treer-agent', source_commit:pins.source_commit, tools}, files};
  const bytes = encode(value); assert.ok(bytes.length <= 256 * 1024 * 1024);
  mkdirSync(output, {recursive:true, mode:0o700});
  const name = `launcher-${platform}.json`; writeFileSync(join(output,name), bytes, {flag:'wx',mode:0o600});
  const metadata = {schema:'treer.launcher-package-stage/v1', source_commit:source, platform, package_url:url(`launcher-${source}`,name), package_sha256:sha(bytes), package_size:bytes.length, native_source_commit:pins.source_commit, native_sha256:sha(binary), trust:'GitHub repository HTTPS and SHA256; not signed or claimed immutable'};
  writeFileSync(join(output,`stage-${platform}.json`),encode(metadata),{flag:'wx',mode:0o600});
  return metadata;
}
export function channel(stages) {
  assert.ok(stages.length > 0 && stages.length <= 5);
  const source = stages[0].source_commit; assert.ok(fullSha(source));
  const entries = {};
  for (const s of stages) {
    assert.equal(s.schema,'treer.launcher-package-stage/v1'); assert.equal(s.source_commit,source);
    assert.ok(platforms.has(s.platform) && !Object.hasOwn(entries,s.platform));
    assert.equal(s.package_url,url(`launcher-${source}`,`launcher-${s.platform}.json`));
    assert.ok(hash(s.package_sha256) && Number.isSafeInteger(s.package_size) && s.package_size > 0 && s.package_size <= 256*1024*1024);
    entries[s.platform] = {package_url:s.package_url,package_sha256:s.package_sha256,package_size:s.package_size};
  }
  return {schema:'treer.launcher-latest/v1',source_commit:source,platforms:entries};
}
export async function publicBytes(input, size) {
  let current = new URL(input);
  assert.equal(current.origin,'https://github.com');
  for (let n=0;n<=5;n++) {
    let r;
    try { r = await fetch(current,{redirect:'manual',signal:AbortSignal.timeout(120000),headers:{'Accept-Encoding':'identity'}}); }
    catch { throw new Error('Public release download failed; temporary redirect URL omitted'); }
    if ([301,302,303,307,308].includes(r.status)) {
      await r.body?.cancel(); assert.ok(n<5,'Too many release redirects');
      current = new URL(r.headers.get('location'),current);
      assert.ok(current.protocol==='https:' && !current.username && !current.password && ['github.com','release-assets.githubusercontent.com','objects.githubusercontent.com'].includes(current.hostname),'Untrusted release redirect');
      continue;
    }
    assert.equal(r.status,200,'Published object unavailable');
    const chunks=[];let total=0;
    for await(const c of r.body || []) {total+=c.length;assert.ok(total<=size,'Published object exceeds pin');chunks.push(c);}
    assert.equal(total,size);return Buffer.concat(chunks,total);
  }
  throw new Error('Release object unavailable');
}
export async function publish(directory, stagePaths) {
  const stages = stagePaths.map(p => JSON.parse(read(p,65536)));
  const value = channel(stages), tag=`launcher-${value.source_commit}`;
  for (const s of stages) {
    const bytes=read(join(directory,`launcher-${s.platform}.json`),256*1024*1024);
    assert.equal(bytes.length,s.package_size);assert.equal(sha(bytes),s.package_sha256);
  }
  // Exclusive local custody: unknown CLI result means inspect actual release; never blindly rerun.
  mkdirSync(join(directory,'PUBLICATION'),{mode:0o700});
  const receipt=(n,v)=>writeFileSync(join(directory,'PUBLICATION',n),encode(v),{flag:'wx',mode:0o600});
  const gh=(n,args)=>{
    receipt(n+'-intent.json',{argv:['gh',...args],retry:false});
    try { const out=execFileSync('gh',args,{encoding:'utf8',stdio:['ignore','pipe','pipe'],timeout:120000});receipt(n+'-ack.json',{exit:0,output:out});return out; }
    catch(e){receipt(n+'-unknown.json',{exit:Number.isInteger(e.status)?e.status:null,unknown:true,retry:false});throw new Error('Publication incomplete; inspect retained operation, do not retry');}
  };
  const notes=`Independent launcher ${value.source_commit}. Default entry only; no running-session/Core update. Native tools remain explicitly path-compatible. GitHub HTTPS + SHA256; not a signed release. See launcher package descriptors.`;
  let existing=false;
  try {execFileSync('gh',['api',`repos/${repo}/releases/tags/${tag}`],{stdio:['ignore','pipe','pipe'],timeout:30000});existing=true;}
  catch(e){assert.ok(String(e.stderr).includes('HTTP 404'),'Version existence UNKNOWN; no mutation');}
  if(!existing) gh('version',['release','create',tag,...stages.map(s=>join(directory,`launcher-${s.platform}.json`)),'--repo',repo,'--target',value.source_commit,'--prerelease','--latest=false','--title',`Launcher ${value.source_commit.slice(0,12)}`,'--notes',notes]);
  for (const s of stages) assert.equal(sha(await publicBytes(s.package_url,s.package_size)),s.package_sha256,'Public package mismatch');
  const pointer=encode(value);assert.ok(pointer.length<=65536);
  const pointerPath=join(directory,'launcher-latest.json');writeFileSync(pointerPath,pointer,{flag:'wx',mode:0o600});
  // Read-only existence check; create only if GitHub explicitly reports 404.
  let exists=true;
  try {execFileSync('gh',['api',`repos/${repo}/releases/tags/launcher-stable`],{stdio:['ignore','pipe','pipe'],timeout:30000});}
  catch(e){assert.ok(String(e.stderr).includes('HTTP 404'),'Channel existence UNKNOWN; no mutation');exists=false;}
  if(!exists) gh('channel-create',['release','create','launcher-stable','--repo',repo,'--target',value.source_commit,'--prerelease','--latest=false','--title','Launcher stable channel','--notes','Mutable launcher-only HTTPS pointer. Does not change Core latest.']);
  gh('channel-upload',['release','upload','launcher-stable',pointerPath,'--clobber','--repo',repo]);
  const served=await publicBytes(url('launcher-stable','launcher-latest.json'),pointer.length);
  assert.equal(sha(served),sha(pointer),'Public launcher pointer mismatch');
  receipt('RESULT.json',{status:'PASS',source_commit:value.source_commit,channel_url:url('launcher-stable','launcher-latest.json'),channel_sha256:sha(pointer),platforms:Object.keys(value.platforms),installed:false,agents_restarted:false});
  return value;
}
if(process.argv[1] && resolve(process.argv[1])===fileURLToPath(import.meta.url)) {
  const [mode,...args]=process.argv.slice(2);
  if(mode==='stage') {
    const o={};for(let i=0;i<args.length;i+=2){assert.ok(['--payload','--native','--binding','--build','--source','--platform','--output'].includes(args[i])&&!Object.hasOwn(o,args[i].slice(2))&&args[i+1]);o[args[i].slice(2)]=args[i+1];}
    console.log(JSON.stringify(stagePackage(o)));
  } else if(mode==='publish') {
    assert.ok(args.length>=2,'Use publish ABS_DIRECTORY STAGE_JSON...');assert.ok(isAbsolute(args[0]));
    console.log(JSON.stringify(await publish(args[0],args.slice(1))));
  } else throw new Error('Use launcher-release.mjs stage --payload ABS --native ABS --binding ABS --build ABS --source SHA --platform PLATFORM --output ABS, or publish ABS_DIRECTORY STAGE_JSON...');
}
