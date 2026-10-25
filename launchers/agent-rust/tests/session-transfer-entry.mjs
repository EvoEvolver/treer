// Real default/fixture executable; only fresh synthetic native files, no provider.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const binary = process.env.RUST_SPIKE_BIN;
assert.ok(binary, 'RUST_SPIKE_BIN required');
const key = value => value.replace(/[^a-zA-Z0-9]/g, '-');
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const entry = (scope, name, bytes) => ({ scope, path: name, encoding: 'base64', content: Buffer.from(bytes).toString('base64'), sha256: digest(Buffer.from(bytes)) });
async function fixture(fn) {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), 'treer-transfer247-')));
  try {
    const cwd = path.join(root, 'source-😀'), home = path.join(root, 'synthetic-claude'), id = randomUUID();
    const native = path.join(home, 'projects', key(cwd));
    await fs.mkdir(cwd, { recursive: true, mode: 0o700 });
    await fs.mkdir(native, { recursive: true, mode: 0o700 });
    const original = Buffer.from(`${JSON.stringify({ type: 'user', sessionId: id, cwd, payload: { cwd }, message: { role: 'user', content: 'SYNTHETIC_ONLY' } })}\n`);
    await fs.writeFile(path.join(native, `${id}.jsonl`), original, { mode: 0o600 });
    const env = { PATH: process.env.PATH, HOME: root, AGENT_UI_CWD: '/not-a-runtime-directory', TREER_AGENT_ID: 'not-a-runtime-identity!' };
    const run = args => spawnSync(binary, ['session', ...args], { env, encoding: 'utf8', maxBuffer: 2 * 1024 * 1024, timeout: 30_000 });
    const exportArgs = ['export', '--harness', 'claude', '--config-dir', home, '--cwd', cwd, '--session-id', id];
    const save = async (bundle, name = 'bundle.json') => { const input = path.join(root, name); await fs.writeFile(input, JSON.stringify(bundle), { mode: 0o600 }); return input; };
    const get = () => { const result = run(exportArgs); assert.equal(result.status, 0, result.stderr); return JSON.parse(result.stdout); };
    const noStages = async () => assert.deepEqual((await fs.readdir(root)).filter(n => n.startsWith('.session-import-')), []);
    await fn({ root, cwd, home, native, id, original, env, run, exportArgs, save, get, noStages });
    assert.deepEqual(await fs.readFile(path.join(native, `${id}.jsonl`)), original, 'source untouched');
  } finally { await fs.rm(root, { recursive: true, force: true }); }
}

test('actual entry: allowlisted export, fresh UUID isolated import, canonical re-export', async () => fixture(async f => {
  await fs.mkdir(path.join(f.native, f.id, 'subagents'), { recursive: true });
  await fs.mkdir(path.join(f.native, 'memory'), { recursive: true });
  await fs.mkdir(path.join(f.cwd, '.claude', 'memory'), { recursive: true });
  const sub = `${JSON.stringify({ sessionId: f.id, session_id: f.id, cwd: f.cwd, type: 'assistant' })}\n`;
  await fs.writeFile(path.join(f.native, f.id, 'subagents', 'agent-a.jsonl'), sub, { mode: 0o600 });
  await fs.writeFile(path.join(f.native, 'memory', 'MEMORY.md'), 'native memory', { mode: 0o600 });
  await fs.writeFile(path.join(f.cwd, 'CLAUDE.md'), 'project instructions');
  await fs.writeFile(path.join(f.cwd, '.claude', 'memory', 'topic.md'), 'project memory');
  await fs.writeFile(path.join(f.cwd, 'secret.txt'), 'must never export');
  await fs.writeFile(path.join(f.home, '.credentials.json'), 'must never read', { mode: 0o000 });
  const bundle = f.get();
  assert.equal(bundle.schema, 'treer.agent-session/v1');
  assert.deepEqual(bundle.files.map(x => `${x.scope}/${x.path}`).sort(), ['session/session.jsonl', 'session/subagents/agent-a.jsonl', 'memory/MEMORY.md', 'project/CLAUDE.md', 'project/.claude/memory/topic.md'].sort());
  for (const file of bundle.files) assert.equal(digest(Buffer.from(file.content, 'base64')), file.sha256);
  const input = await f.save(bundle), destination = path.join(f.root, 'restored');
  const result = f.run(['import', '--input', input, '--directory', destination]);
  assert.equal(result.status, 0, result.stderr);
  const imported = JSON.parse(result.stdout);
  assert.notEqual(imported.sessionId, f.id);
  assert.match(imported.sessionId, /^[0-9a-f-]{36}$/);
  assert.equal(imported.cwd, destination);
  const config = path.join(destination, '.harness', 'claude');
  assert.deepEqual(imported.environment, { CLAUDE_CONFIG_DIR: config });
  const target = path.join(config, 'projects', key(destination));
  const row = JSON.parse((await fs.readFile(path.join(target, `${imported.sessionId}.jsonl`), 'utf8')).trim());
  assert.equal(row.sessionId, imported.sessionId); assert.equal(row.cwd, destination); assert.equal(row.payload.cwd, destination);
  const subRow = JSON.parse((await fs.readFile(path.join(target, imported.sessionId, 'subagents', 'agent-a.jsonl'), 'utf8')).trim());
  assert.equal(subRow.session_id, imported.sessionId);
  assert.equal((await fs.stat(path.join(target, `${imported.sessionId}.jsonl`))).mode & 0o777, 0o600);
  assert.equal((await fs.stat(destination)).mode & 0o777, 0o700);
  assert.equal(await fs.readFile(path.join(destination, 'CLAUDE.md'), 'utf8'), 'project instructions');
  assert.equal(await fs.readFile(path.join(target, 'memory', 'MEMORY.md'), 'utf8'), 'native memory');
  assert.equal(await fs.stat(path.join(config, '.credentials.json')).catch(() => null), null);
  const again = f.run(['export', '--harness', 'claude', '--config-dir', config, '--cwd', destination, '--session-id', imported.sessionId]);
  assert.equal(again.status, 0, again.stderr); assert.equal(JSON.parse(again.stdout).files.length, bundle.files.length);
  await f.noStages();
}));

test('actual entry: inspect is readonly; hash-bound prepare publishes only reviewed bytes', async () => fixture(async f => {
  const input = await f.save(f.get()), bytes = await fs.readFile(input), expected = digest(bytes);
  const destination = path.join(f.root, 'reviewed-target'), id = randomUUID();
  const options = ['--input', input, '--directory', destination, '--session-id', id, '--sha256', expected];
  const inspection = f.run(['inspect', ...options]);
  assert.equal(inspection.status, 0, inspection.stderr);
  const checked = JSON.parse(inspection.stdout);
  assert.equal(checked.schema, 'treer.claude-prepared/v1');
  assert.equal(checked.phase, 'validated');
  assert.equal(checked.inputSha256, expected);
  assert.equal(checked.sourceSessionId, f.id);
  assert.equal(checked.sessionId, id);
  assert.equal(checked.cwd, destination);
  assert.ok(checked.files > 0 && checked.bytes > 0);
  await assert.rejects(fs.stat(destination));await f.noStages();
  // Even a semantically valid change between review and publication refuses.
  const changed = JSON.parse(bytes);changed.exportedAt = new Date().toISOString() + '-changed';
  await fs.writeFile(input, JSON.stringify(changed));
  const refused = f.run(['prepare', ...options]);
  assert.notEqual(refused.status, 0);assert.match(refused.stderr, /snapshot_sha256_mismatch/);
  await assert.rejects(fs.stat(destination));await f.noStages();
  await fs.writeFile(input, bytes);
  const result = f.run(['prepare', ...options]);assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), {...checked, phase: 'prepared'});
  assert.equal(JSON.parse(await fs.readFile(path.join(destination, '.harness/claude-launch.json'), 'utf8')).sessionId, id);
  const duplicate = f.run(['prepare', ...options]);assert.notEqual(duplicate.status, 0);
  assert.match(duplicate.stderr, /destination_must_be_new/);
}));

test('actual entry: inspection validates paths, content, native history and new identity before publication', async () => fixture(async f => {
  const destination = path.join(f.root, 'never-published'), id = randomUUID(), bundle = f.get();
  async function refuses(value, extra = [], pattern) {
    const input = await f.save(value), options = ['--input', input, '--directory', destination, '--session-id', id, '--sha256', digest(await fs.readFile(input))];
    const result = f.run(['inspect', ...options, ...extra]);assert.notEqual(result.status, 0);
    if (pattern) assert.match(result.stderr, pattern);
    await assert.rejects(fs.stat(destination));await f.noStages();
  }
  await refuses({...bundle, files: bundle.files.map((e, i) => i === 0 ? {...e, path:'../outside'} : e)});
  await refuses({...bundle, files: bundle.files.map((e, i) => i === 0 ? {...e, sha256:'0'.repeat(64)} : e)});
  await refuses({...bundle, files:[entry('session','session.jsonl',`${JSON.stringify({sessionId:f.id,type:'system'})}\n`)]});
  await refuses(bundle, ['--sha256', '0'.repeat(64)], /unknown_or_duplicate_session_option/);
  const input=await f.save(bundle), base=['--input',input,'--directory',destination,'--sha256',digest(await fs.readFile(input))];
  assert.notEqual(f.run(['inspect',...base]).status,0,'explicit fresh UUID required');
  assert.notEqual(f.run(['inspect',...base,'--session-id',f.id]).status,0,'source UUID never reused');
  await fs.chmod(input,0o644);
  assert.match(f.run(['inspect',...base,'--session-id',id]).stderr,/snapshot_private_storage_required/);
  await fs.chmod(input,0o600);await fs.chmod(f.root,0o755);
  try {assert.match(f.run(['inspect',...base,'--session-id',id]).stderr,/snapshot_private_storage_required/);}
  finally {await fs.chmod(f.root,0o700);}
  await fs.mkdir(destination,{mode:0o700});
  assert.match(f.run(['inspect',...base,'--session-id',id]).stderr,/destination_must_be_new/);
  assert.deepEqual(await fs.readdir(destination),[]);
}));

test('actual entry: malicious paths/hash/base64/duplicates/JSONL rejected before publication', async () => fixture(async f => {
  const bundle = f.get();
  const cases = [
    b => { b.files[0].path = '../outside'; }, b => { b.files[0].path = '/outside'; },
    b => { b.files[0].path = 'subagents\\bad.jsonl'; }, b => { b.files[0].path = 'subagents/C:bad.jsonl'; },
    b => { b.files[0].sha256 = '0'.repeat(64); }, b => { b.files[0].content += '\n'; },
    b => { b.files.push(b.files[0]); }, b => { b.files.push(entry('project', '.credentials.json', 'forbidden')); },
    b => { b.files[0] = entry('session', 'session.jsonl', '{"sessionId":"a","sessionId":"b"}\n'); },
    b => { b.files[0] = entry('session', 'session.jsonl', JSON.stringify({ sessionId: f.id })); },
    b => { b.files[0] = entry('session', 'session.jsonl', `${JSON.stringify({ sessionId: randomUUID() })}\n`); },
    b => { b.files = Array.from({ length: 1025 }, () => b.files[0]); },
  ];
  for (const [i, change] of cases.entries()) {
    const copy = structuredClone(bundle); change(copy);
    const input = await f.save(copy), destination = path.join(f.root, `bad-${i}`);
    const result = f.run(['import', '--input', input, '--directory', destination]);
    assert.notEqual(result.status, 0, `case ${i}`);
    assert.equal(await fs.stat(destination).catch(() => null), null); await f.noStages();
  }
  const input = await f.save(bundle);
  for (const id of [f.id, f.id.toUpperCase(), '../escape']) {
    assert.notEqual(f.run(['import', '--input', input, '--directory', path.join(f.root, 'bad-id'), '--session-id', id]).status, 0);
  }
}));

test('actual entry: refuses existing empty/nonempty/link targets; staged write failure cleans only own stage', async () => fixture(async f => {
  const bundle = f.get(), input = await f.save(bundle);
  for (const kind of ['empty', 'nonempty', 'symlink']) {
    const destination = path.join(f.root, kind);
    if (kind === 'symlink') await fs.symlink(f.cwd, destination);
    else { await fs.mkdir(destination); if (kind === 'nonempty') await fs.writeFile(path.join(destination, 'keep'), 'keep'); }
    assert.notEqual(f.run(['import', '--input', input, '--directory', destination]).status, 0);
    assert.ok(await fs.lstat(destination)); await f.noStages();
  }
  // Both allowlisted, but a regular file cannot also be a directory. Fails
  // after staging writes (not just validation), proving cleanup/no publication.
  bundle.files.push(entry('memory', 'x.md', 'first'), entry('memory', 'x.md/y.md', 'second'));
  const collision = await f.save(bundle), destination = path.join(f.root, 'collision');
  const result = f.run(['import', '--input', collision, '--directory', destination]);
  assert.notEqual(result.status, 0); assert.equal(await fs.stat(destination).catch(() => null), null); await f.noStages();
}));

test('actual entry: source symlink/hardlink/FIFO and incomplete JSONL refused', async () => fixture(async f => {
  const file = path.join(f.native, `${f.id}.jsonl`), saved = path.join(f.root, 'saved.jsonl');
  await fs.rename(file, saved); await fs.symlink(saved, file);
  assert.notEqual(f.run(f.exportArgs).status, 0); await fs.unlink(file);
  await fs.link(saved, file); assert.notEqual(f.run(f.exportArgs).status, 0); await fs.unlink(file);
  assert.equal(spawnSync('/usr/bin/mkfifo', [file]).status, 0);
  const fifo = f.run(f.exportArgs); assert.notEqual(fifo.status, 0); assert.equal(fifo.error, undefined, 'nonblocking FIFO refusal');
  await fs.unlink(file); await fs.rename(saved, file);
  await fs.writeFile(file, f.original.subarray(0, -1)); assert.notEqual(f.run(f.exportArgs).status, 0);
  await fs.writeFile(file, f.original);
  const moved = `${f.native}-moved`;
  await fs.rename(f.native, moved); await fs.symlink(moved, f.native);
  assert.notEqual(f.run(f.exportArgs).status, 0, 'no symlinked native project ancestor');
  await fs.unlink(f.native); await fs.rename(moved, f.native);
  await fs.chmod(f.native, 0o777);
  assert.notEqual(f.run(f.exportArgs).status, 0, 'no writable native ancestor');
  await fs.chmod(f.native, 0o700);
}));

test('actual entry: 64MiB native and 96MiB input bounds refuse before reads', async () => fixture(async f => {
  const file = path.join(f.native, `${f.id}.jsonl`);
  await fs.truncate(file, 64 * 1024 * 1024 + 1);
  assert.notEqual(f.run(f.exportArgs).status, 0); await fs.writeFile(file, f.original);
  const oversized = path.join(f.root, 'oversized.json');
  await fs.writeFile(oversized, '', { mode: 0o600 }); await fs.truncate(oversized, 96 * 1024 * 1024 + 1);
  assert.notEqual(f.run(['import', '--input', oversized, '--directory', path.join(f.root, 'too-large')]).status, 0);
  const bundle = f.get();
  bundle.files.push(entry('memory', 'oversized.md', Buffer.alloc(64 * 1024 * 1024)));
  const input = await f.save(bundle);
  assert.notEqual(f.run(['import', '--input', input, '--directory', path.join(f.root, 'too-large-decoded')]).status, 0);
  await f.noStages();
}));

test('actual entry: concurrent importers publish exactly one complete new directory', async () => fixture(async f => {
  const input = await f.save(f.get()), destination = path.join(f.root, 'winner');
  const results = await Promise.all(Array.from({ length: 4 }, () => new Promise((resolve, reject) => {
    const child = spawn(binary, ['session', 'import', '--input', input, '--directory', destination], { env: f.env, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = ''; child.stdout.on('data', b => { stdout += b; }); child.stderr.resume();
    child.on('error', reject); child.on('exit', status => resolve({ status, stdout }));
  })));
  assert.equal(results.filter(r => r.status === 0).length, 1);
  const winner = JSON.parse(results.find(r => r.status === 0).stdout);
  const file = path.join(winner.environment.CLAUDE_CONFIG_DIR, 'projects', key(destination), `${winner.sessionId}.jsonl`);
  assert.equal(JSON.parse((await fs.readFile(file, 'utf8')).trim()).sessionId, winner.sessionId);
  await f.noStages();
}));

test('actual installed-dispatcher entry: session goes to verified native image without provider', async () => fixture(async f => {
  const bundleRoot = path.join(f.root, 'bound-launcher'), scripts = path.join(bundleRoot, 'scripts'), publicDir = path.join(bundleRoot, 'public');
  await fs.mkdir(scripts, { recursive: true, mode: 0o700 }); await fs.mkdir(publicDir);
  for (const name of ['treer-agent.mjs', 'native-runtime.mjs', 'process.mjs']) {
    await fs.copyFile(fileURLToPath(new URL(`../../agent/scripts/${name}`, import.meta.url)), path.join(scripts, name));
  }
  const image = path.join(bundleRoot, 'native'); await fs.copyFile(binary, image); await fs.chmod(image, 0o700);
  const bind = async name => { const bytes = await fs.readFile(name); return { path: name, bytes: bytes.length, sha256: digest(bytes) }; };
  const assets = [];
  for (const name of ['index.html', 'boot.js', 'ui.js', 'ui.css']) { const file = path.join(publicDir, name); await fs.writeFile(file, 'synthetic'); assets.push(await bind(file)); }
  const imageBinding = await bind(image);
  await fs.writeFile(path.join(bundleRoot, 'native-runtime.json'), JSON.stringify({ schema: 'treer.native-launcher/v1', platform: process.platform, arch: process.arch, source_commit: '1'.repeat(40), executable: imageBinding, public_dir: publicDir, assets, tools: { treer: { ...imageBinding, version: 'synthetic-not-invoked' } } }));
  const result = spawnSync(process.execPath, [path.join(scripts, 'treer-agent.mjs'), 'session', ...f.exportArgs], { env: f.env, encoding: 'utf8', timeout: 30_000, maxBuffer: 2 * 1024 * 1024 });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).sessionId, f.id);
  // Exercise the ordinary (pre-offline CLI) main path with the dispatcher's
  // fail-closed environment: it must refuse before runtime/provider startup.
  const runtimeFallback = spawnSync(image, [], { env: { ...f.env, AGENT_UI_CWD: image, AGENT_UI_HARNESS: 'offline-session' }, encoding: 'utf8', timeout: 5000 });
  assert.notEqual(runtimeFallback.status, 0); assert.equal(runtimeFallback.error, undefined);
  assert.match(runtimeFallback.stderr, /workspace_file_root_unavailable/);
  // A changed image must refuse, never take the explicit Node launcher path.
  await fs.appendFile(image, 'changed');
  const changed = spawnSync(process.execPath, [path.join(scripts, 'treer-agent.mjs'), 'session', '--help'], { env: f.env, encoding: 'utf8', timeout: 30_000 });
  assert.notEqual(changed.status, 0); assert.match(changed.stderr, /Native launcher binding/);
}));

test('actual Node compatibility: Node bundle -> Rust isolated restore -> Node export', { skip: !process.env.TREER_TRANSFER_NODE_LOADER }, async () => fixture(async f => {
  const module = new URL('../../agent/src/harness/session-files.ts', import.meta.url).href;
  const invoke = (source, args) => spawnSync(process.execPath, ['--import', process.env.TREER_TRANSFER_NODE_LOADER, '--input-type=module', '-e', source, ...args], { env: { PATH: process.env.PATH, HOME: f.root }, encoding: 'utf8', timeout: 30_000, maxBuffer: 2 * 1024 * 1024 });
  const exported = invoke(`import { exportNativeSession } from ${JSON.stringify(module)}; console.log(JSON.stringify(await exportNativeSession('claude', ...process.argv.slice(1))));`, [f.id, f.cwd, f.home]);
  assert.equal(exported.status, 0, exported.stderr);
  const input = await f.save(JSON.parse(exported.stdout)), destination = path.join(f.root, 'node-compatible');
  const result = f.run(['import', '--input', input, '--directory', destination]); assert.equal(result.status, 0, result.stderr);
  const imported = JSON.parse(result.stdout);
  const reexported = invoke(`import { exportNativeSession } from ${JSON.stringify(module)}; console.log(JSON.stringify(await exportNativeSession('claude', ...process.argv.slice(1))));`, [imported.sessionId, destination, imported.environment.CLAUDE_CONFIG_DIR]);
  assert.equal(reexported.status, 0, reexported.stderr);
  const bundle = JSON.parse(reexported.stdout); assert.equal(bundle.files.length, 1); assert.equal(bundle.sessionId, imported.sessionId);
  // Node also accepts a Rust export, using only a second synthetic provider home.
  const rustBundle = await f.save(f.get(), 'rust-node-input.json'), nodeHome = path.join(f.root, 'node-import-home');
  const nodeImported = invoke(`import { readFile } from 'node:fs/promises'; import { importNativeSession } from ${JSON.stringify(module)}; const [input,directory,home]=process.argv.slice(1); console.log(JSON.stringify(await importNativeSession(JSON.parse(await readFile(input,'utf8')), {directory}, home)));`, [rustBundle, path.join(f.root, 'node-restored'), nodeHome]);
  assert.equal(nodeImported.status, 0, nodeImported.stderr); assert.notEqual(JSON.parse(nodeImported.stdout).sessionId, f.id);
}));
