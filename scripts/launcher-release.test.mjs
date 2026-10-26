import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync,mkdirSync,writeFileSync,readFileSync,symlinkSync,rmSync,realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { stagePackage,channel,publish } from './launcher-release.mjs';
const hash=b=>createHash('sha256').update(b).digest('hex');
function fixture(t) {
 const r=realpathSync.native(mkdtempSync(join(tmpdir(),'launcher-publication249-')));t.after(()=>rmSync(r,{recursive:true,force:true}));
 const payload=join(r,'payload');mkdirSync(join(payload,'scripts'),{recursive:true});
 const source='a'.repeat(40),runtime='b'.repeat(40);
 const paths=['runtime.mjs','interface-contract.json','profiles.json','scripts/install.mjs','scripts/installed-runner.mjs','scripts/treer-agent.mjs'];
 const files=paths.map(path=>{const b=Buffer.from('{}\n');writeFileSync(join(payload,path),b);return {path,size:b.length,sha256:hash(b),executable:false};});
 writeFileSync(join(payload,'bundle-manifest.json'),JSON.stringify({schema:'treer.launcher-payload/v1',source_commit:source,node_min:22,entry:'runtime.mjs',runtime_ready:true,files}));
 const binary=Buffer.from('7f454c4601020304','hex'),native=join(r,'native');writeFileSync(native,binary);
 const tools={treer:{path:'/opt/treer',version:'1',sha256:'c'.repeat(64),bytes:10},codex:{path:'/opt/codex',version:'2',sha256:'d'.repeat(64),bytes:20}};
 const binding=join(r,'binding.json');writeFileSync(binding,JSON.stringify({schema:'treer.native-launcher/v1',source_commit:runtime,platform:'linux',arch:'x64',executable:{sha256:hash(binary),bytes:binary.length},tools}));
 const build=join(r,'build.json');writeFileSync(build,JSON.stringify({schema:'treer.launcher-native-build/v1',source_commit:runtime,platform:'linux-x86_64',ordinary:true,fixture_harness:false,sha256:hash(binary),bytes:binary.length,tool_paths:{treer:'/opt/treer',codex:'/opt/codex'}}));
 return {payload,native,binding,build,source,platform:'linux-x86_64',output:join(r,'out')};
}
test('package preserves native provenance and exact file bytes; no runtime machine manifest',t=>{
 const o=fixture(t),s=stagePackage(o),p=JSON.parse(readFileSync(join(o.output,'launcher-linux-x86_64.json')));
 assert.equal(p.native.source_commit,'b'.repeat(40));assert.equal(p.source_commit,o.source);
 assert.equal(p.files.length,8);assert.ok(!p.files.some(f=>f.path.endsWith('native-runtime.json')));
 for(const f of p.files){const b=Buffer.from(f.data_base64,'base64');assert.equal(b.length,f.size);assert.equal(hash(b),f.sha256);}
 assert.equal(hash(readFileSync(join(o.output,'launcher-linux-x86_64.json'))),s.package_sha256);
 assert.equal(channel([s]).platforms['linux-x86_64'].package_url,s.package_url);
});
test('reject native tamper, fixture declaration and mismatched compiled tools',t=>{
 for(const kind of ['tamper','fixture','path']) {
  const o=fixture(t);
  if(kind==='tamper')writeFileSync(o.native,Buffer.from('tampered'));
  else {const p=JSON.parse(readFileSync(o.build));if(kind==='fixture')p.fixture_harness=true;else p.tool_paths.codex='/other/codex';writeFileSync(o.build,JSON.stringify(p));}
  assert.throws(()=>stagePackage(o));
 }
});
test('reject unlisted payload file and symlink rather than publishing them',t=>{
 for(const kind of ['extra','symlink']) {
  const o=fixture(t);if(kind==='extra')writeFileSync(join(o.payload,'extra'), 'no');else {rmSync(join(o.payload,'runtime.mjs'));symlinkSync(o.native,join(o.payload,'runtime.mjs'));}
  assert.throws(()=>stagePackage(o));
 }
});
test('reject installed native-runtime even when included in valid manifest',t=>{
 const o=fixture(t),p=join(o.payload,'bundle-manifest.json'),m=JSON.parse(readFileSync(p)),bytes=Buffer.from('{}');writeFileSync(join(o.payload,'native-runtime.json'),bytes);m.files.push({path:'native-runtime.json',size:2,sha256:hash(bytes),executable:false});writeFileSync(p,JSON.stringify(m));assert.throws(()=>stagePackage(o),/Machine-specific/);
});
test('channel refuses duplicate platforms, mixed source and foreign URL',t=>{
 const s=stagePackage(fixture(t));assert.throws(()=>channel([s,s]));assert.throws(()=>channel([s,{...s,platform:'darwin-aarch64',source_commit:'e'.repeat(40)}]));assert.throws(()=>channel([{...s,package_url:'https://evil.invalid/file'}]));
});
test('publication detects package tamper before creating any intent or invoking gh',async t=>{
 const o=fixture(t),s=stagePackage(o);writeFileSync(join(o.output,'launcher-linux-x86_64.json'),'changed');
 await assert.rejects(publish(o.output,[join(o.output,'stage-linux-x86_64.json')]));
 assert.equal(s.source_commit,o.source);
});
