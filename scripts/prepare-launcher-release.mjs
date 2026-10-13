#!/usr/bin/env node
// Run only after lockfile-pinned npm ci --ignore-scripts in the trusted build checkout.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { appendFileSync, lstatSync, readFileSync, readdirSync } from 'node:fs';
import { isAbsolute, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export function verifyBundle(directory, commit) {
  assert.ok(isAbsolute(directory));
  assert.match(commit, /^[a-f0-9]{40}$/);
  assert.ok(lstatSync(directory).isDirectory() && !lstatSync(directory).isSymbolicLink());
  const found = new Map();
  function walk(path, prefix = '') {
    for (const name of readdirSync(path)) {
      const relative = prefix + name, full = join(path, name), stat = lstatSync(full);
      assert.ok(!stat.isSymbolicLink(), 'Payload links are forbidden');
      if (stat.isDirectory()) walk(full, relative + '/');
      else { assert.ok(stat.isFile(), 'Payload special files are forbidden'); found.set(relative, full); }
    }
  }
  walk(directory);
  const manifestPath = found.get('bundle-manifest.json');
  assert.ok(manifestPath, 'Missing bundle manifest');
  const bytes = readFileSync(manifestPath), manifest = JSON.parse(bytes);
  assert.equal(manifest.schema, 'treer.launcher-payload/v1');
  assert.equal(manifest.source_commit, commit);
  assert.equal(manifest.node_min, 22);
  assert.equal(manifest.entry, 'runtime.mjs');
  assert.equal(manifest.runtime_ready, true);
  assert.ok(Array.isArray(manifest.files));
  const names = new Set();
  for (const file of manifest.files) {
    assert.equal(typeof file.path, 'string');
    assert.ok(file.path.split('/').every(p => p && p !== '.' && p !== '..' && !/[\\:\x00-\x1f]/.test(p)), 'Unsafe payload path');
    assert.ok(file.path !== 'bundle-manifest.json' && !names.has(file.path));
    assert.ok(!file.path.split('/').includes('node_modules'));
    names.add(file.path);
    assert.equal(typeof file.executable, 'boolean');
    assert.ok(Number.isSafeInteger(file.size) && file.size >= 0);
    assert.match(file.sha256, /^[a-f0-9]{64}$/);
    assert.ok(found.has(file.path), 'Missing payload file');
    const content = readFileSync(found.get(file.path));
    assert.equal(content.length, file.size);
    assert.equal(createHash('sha256').update(content).digest('hex'), file.sha256);
  }
  assert.ok(names.has('runtime.mjs'), 'Missing portable runtime');
  assert.equal(found.size, names.size + 1, 'Unlisted payload file');
  return 'sha256-' + createHash('sha256').update(bytes).digest('hex');
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  assert.ok(Number(process.versions.node.split('.')[0]) >= 22, 'Node 22+ required');
  const args = process.argv.slice(2);
  assert.equal(args.length, 4, 'Use --output ABS --source-commit FULL_SHA');
  assert.equal(args[0], '--output'); assert.equal(args[2], '--source-commit');
  const directory = args[1], commit = args[3];
  assert.ok(isAbsolute(directory) && !/[\r\n]/.test(directory));
  assert.match(commit, /^[a-f0-9]{40}$/);
  const root = fileURLToPath(new URL('../', import.meta.url));
  const result = spawnSync(process.execPath, [join(root, 'launchers/agent/scripts/prepare-bundle.mjs'), ...args], { cwd: root, stdio: 'inherit' });
  assert.equal(result.status, 0, 'Launcher bundle preparation failed');
  const version = verifyBundle(directory, commit);
  if (process.env.GITHUB_ENV) appendFileSync(process.env.GITHUB_ENV, `TREER_LAUNCHER_PAYLOAD_DIR=${directory}\nTREER_REQUIRE_LAUNCHER_BUNDLE=1\n`);
  console.log(JSON.stringify({ source_commit: commit, version, runtime_ready: true }));
}
