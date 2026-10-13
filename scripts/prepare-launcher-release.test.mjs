import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { verifyBundle } from './prepare-launcher-release.mjs';
const commit = 'a'.repeat(40);
function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'payload-gate-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const bytes = Buffer.from('console.log("fixture")');
  writeFileSync(join(directory, 'runtime.mjs'), bytes);
  const manifest = { schema: 'treer.launcher-payload/v1', source_commit: commit, node_min: 22, entry: 'runtime.mjs', runtime_ready: true, files: [{ path: 'runtime.mjs', size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex'), executable: false }] };
  const save = () => writeFileSync(join(directory, 'bundle-manifest.json'), JSON.stringify(manifest));
  save(); return { directory, manifest, save };
}
test('version identifies exact verified manifest bytes', t => {
  const f = fixture(t); assert.match(verifyBundle(f.directory, commit), /^sha256-[a-f0-9]{64}$/);
  assert.throws(() => verifyBundle(f.directory, 'b'.repeat(40)));
  writeFileSync(join(f.directory, 'runtime.mjs'), 'corrupt');
  assert.throws(() => verifyBundle(f.directory, commit));
});
test('official payload rejects missing runtime, unlisted bytes and unsafe paths', t => {
  for (const mutate of [
    f => { f.manifest.files = []; f.save(); },
    f => writeFileSync(join(f.directory, 'extra'), 'unlisted'),
    f => { f.manifest.runtime_ready = false; f.save(); },
    f => { f.manifest.files[0].path = '../runtime.mjs'; f.save(); },
    f => { f.manifest.files.push({ ...f.manifest.files[0] }); f.save(); },
    f => { f.manifest.node_min = 18; f.save(); },
    f => { f.manifest.entry = 'source.ts'; f.save(); },
    f => { mkdirSync(join(f.directory, 'node_modules')); writeFileSync(join(f.directory, 'node_modules', 'extra'), 'x'); },
  ]) { const f = fixture(t); mutate(f); assert.throws(() => verifyBundle(f.directory, commit)); }
});
test('payload links cannot substitute bytes', { skip: process.platform === 'win32' }, t => {
  const f = fixture(t); rmSync(join(f.directory, 'runtime.mjs'));
  writeFileSync(join(f.directory, 'target'), 'console.log("fixture")');
  symlinkSync(join(f.directory, 'target'), join(f.directory, 'runtime.mjs'));
  assert.throws(() => verifyBundle(f.directory, commit));
});
