// Artifact check only: the native provider must never be started by this test.
import assert from 'node:assert/strict';
import test from 'node:test';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, realpath, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { hermeticEnv } from '../../agent/tests/hermetic.mjs';

test('default release artifact excludes HTTP fixture hooks and refuses provider overrides', async t => {
  const binary = process.env.RUST_SPIKE_RELEASE_BIN;
  assert.ok(binary, 'RUST_SPIKE_RELEASE_BIN must name a default-feature release build');
  const bytes = await readFile(binary);
  for (const hook of ['RUST_SPIKE_HTTP_', 'RUST_SPIKE_PROVIDER_LOG']) {
    assert.equal(bytes.includes(Buffer.from(hook)), false, `test-only hook remains in release: ${hook}`);
  }
  const root = await realpath(await mkdtemp(join(tmpdir(), 'rust158-release-')));
  t.after(() => rm(root, { recursive: true, force: true }));
  for (const override of [{ CODEX_BIN: '/synthetic-do-not-start' }, { RUST_SPIKE_CODEX_ARGS: '[]' }]) {
    const result = spawnSync(binary, [], {
      cwd: root,
      env: hermeticEnv({ HOME: root, CODEX_HOME: join(root, 'codex'), AGENT_UI_CWD: root, ...override }),
      encoding: 'utf8', timeout: 5000,
    });
    assert.equal(result.status, 1, result.error?.message);
    assert.match(result.stderr, /harness overrides require the fixture-harness test feature/);
    assert.doesNotMatch(result.stderr, /synthetic-do-not-start/);
  }
  t.diagnostic(JSON.stringify({ sha256: createHash('sha256').update(bytes).digest('hex'), bytes: bytes.length }));
});
