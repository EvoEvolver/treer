import assert from 'node:assert/strict';
import test from 'node:test';
import {spawnSync} from 'node:child_process';
import {mkdtempSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';

test('deployment paths bind independently of native build images without allowing missing inputs',t=>{
  const dir=mkdtempSync(join(tmpdir(),'rust-build-binding-'));
  t.after(()=>rmSync(dir,{recursive:true,force:true}));
  const bin=join(dir,'build-script');
  const compiled=spawnSync('rustc',[resolve('build.rs'),'-o',bin],{encoding:'utf8'});
  assert.equal(compiled.status,0,compiled.stderr);
  const env={...process.env,TREER_SPIKE_BUILD_CODEX:'/usr/bin/true',TREER_SPIKE_BUILD_CLAUDE:'/usr/bin/true',TREER_SPIKE_TARGET_CODEX:'/home/target/bin/codex',TREER_SPIKE_TARGET_TREER:'/home/target/bin/treer',TREER_SPIKE_TARGET_CLAUDE:'/home/target/bin/claude'};
  const run=extra=>spawnSync(bin,[],{env:{...env,...extra},encoding:'utf8'});
  const ok=run({});assert.equal(ok.status,0,ok.stderr);
  for(const tool of ['CODEX','TREER','CLAUDE'])assert.ok(ok.stdout.includes(`cargo:rustc-env=SPIKE_${tool}_PATH=/home/target/bin/${tool.toLowerCase()}\n`));
  for(const bad of ['relative/codex','/home/target/../codex','/home/target/codex\nother'])assert.notEqual(run({TREER_SPIKE_TARGET_CODEX:bad}).status,0);
  assert.notEqual(run({TREER_SPIKE_BUILD_CODEX:'/nonexistent-native-input'}).status,0);
  assert.notEqual(run({TREER_SPIKE_BUILD_CODEX:resolve('../agent/tests/fake-codex.mjs')}).status,0,'script input cannot become a production provider');
  const windows={CARGO_CFG_TARGET_OS:'windows',TREER_SPIKE_TARGET_CODEX:'C:\\Treer\\codex.exe',TREER_SPIKE_TARGET_TREER:'C:\\Treer\\treer.exe',TREER_SPIKE_TARGET_CLAUDE:'C:\\Treer\\claude.exe'};
  assert.equal(run(windows).status,0);
  for(const path of ['C:codex.exe','C:\\Treer\\..\\codex.exe','C:\\Treer\\codex.cmd','C:\\Treer\\file:stream.exe','\\\\server\\share\\codex.exe','C:\\Treer\\codex.exe.'])
    assert.notEqual(run({...windows,TREER_SPIKE_TARGET_CODEX:path}).status,0,path);
});
