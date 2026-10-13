import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

const source = readFileSync(new URL('../crates/treer-proxy/src/api/machines.rs', import.meta.url), 'utf8');
const template = source.split('pub(super) fn render_install_script(')[1].match(/r#"([\s\S]*?)"#,/)[1];
const binaries = ['treer', 'treer-agent-host', 'treer-agent-server'];
const payload = Object.fromEntries(binaries.map(name => [name, `#!/bin/sh\n# ${name}\ntouch "$HOME/binary-ran"\n`]));
const sums = binaries.map(name => `${createHash('sha256').update(payload[name]).digest('hex')}  ${name}\n`).join('');

function render(base) {
  // Exercise the actual generated script, including the two embedded shell files.
  const script = template.replaceAll('{{', '{').replaceAll('}}', '}')
    .replace('{artifact_base}', `'${base}'`)
    .replace('{checksum_setup}', readFileSync(new URL('../crates/treer-proxy/src/install-checksums.sh', import.meta.url), 'utf8'))
    .replace('{path_setup}', readFileSync(new URL('../crates/treer-proxy/src/install-path.sh', import.meta.url), 'utf8'));
  assert.ok(source.includes('checksum_setup = include_str!("../install-checksums.sh")'));
  return script;
}

async function fixture(t, options = {}) {
  const home = mkdtempSync(join(tmpdir(), 'treer-checksums-'));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const cli = join(home, 'bin with spaces');
  const serverDir = join(home, 'libexec');
  mkdirSync(cli); mkdirSync(serverDir);
  for (const name of binaries) writeFileSync(join(name === 'treer' ? cli : serverDir, name), `old ${name}`);
  const server = createServer((request, response) => {
    const name = request.url.split('/').at(-1);
    if (name === 'SHA256SUMS') {
      if (options.missing) { response.writeHead(404); response.end(); return; }
      response.end(options.sums ?? sums); return;
    }
    if (!binaries.includes(name)) { response.writeHead(404); response.end(); return; }
    if (options.cutoff === name) {
      response.writeHead(200, { 'Content-Length': Buffer.byteLength(payload[name]) + 100 });
      response.end(payload[name].slice(0, 10)); return;
    }
    response.end(options.corrupt === name ? payload[name].slice(0, 10) : payload[name]);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  const script = render(`http://127.0.0.1:${server.address().port}/artifacts`);
  const result = await new Promise((resolve, reject) => {
    const child = spawn('/bin/sh', ['-eu'], {
      env: { HOME: home, TMPDIR: home, PATH: '/usr/bin:/bin', SHELL: '/bin/sh',
        TREER_INSTALL_DIR: cli, TREER_AGENT_SERVER_INSTALL_DIR: serverDir, TREER_INSTALL_MODIFY_PATH: '0' },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let stdout = '', stderr = '';
    child.stdout.on('data', chunk => stdout += chunk);
    child.stderr.on('data', chunk => stderr += chunk);
    child.on('error', reject);
    child.on('close', status => resolve({ status, stdout, stderr }));
    child.stdin.end(script);
  });
  assert.equal(existsSync(join(home, 'binary-ran')), false, 'downloaded binaries must never execute');
  return { result, cli, serverDir };
}

test('verified complete set installs without executing any binary', async t => {
  const f = await fixture(t);
  assert.equal(f.result.status, 0, f.result.stderr);
  for (const name of binaries) assert.equal(readFileSync(join(name === 'treer' ? f.cli : f.serverDir, name), 'utf8'), payload[name]);
});

const failures = [
  ...binaries.map(name => [`corrupted ${name}`, { corrupt: name }]),
  ['missing SHA256SUMS', { missing: true }],
  ['truncated binary HTTP response', { cutoff: 'treer-agent-server' }],
  ['truncated SHA256SUMS', { sums: sums.slice(0, -20) }],
  ['missing binary checksum', { sums: sums.split('\n').slice(0, 2).join('\n') }],
  ['duplicate checksum entry', { sums: sums + sums.split('\n')[0] + '\n' }],
  ['invalid hash', { sums: sums.replace(/^[a-f0-9]/, 'z') }],
];
for (const [description, options] of failures) test(`${description} fails before replacing any installed binary`, async t => {
  const f = await fixture(t, options);
  assert.notEqual(f.result.status, 0, f.result.stdout);
  for (const name of binaries) assert.equal(readFileSync(join(name === 'treer' ? f.cli : f.serverDir, name), 'utf8'), `old ${name}`);
});
