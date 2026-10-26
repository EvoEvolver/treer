// Provision only the public authority shipped in this reviewed source bundle.
// Existing keys are never replaced; no network or machine lifecycle operation.
import assert from 'node:assert/strict';
import { createHash, createPublicKey } from 'node:crypto';
import { lstatSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { isAbsolute, join, parse, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

export function installReleaseTrust(directory) {
  assert.ok(isAbsolute(directory) && resolve(directory) === directory && !/[\x00-\x1f\x7f]/.test(directory), 'Select a physical absolute config directory');
  let parent = parse(directory).root;
  for (const part of relative(parent, directory).split(sep).filter(Boolean)) {
    parent = join(parent, part);
    let info;
    try { info = lstatSync(parent); }
    catch (error) { if (error.code !== 'ENOENT') throw error; mkdirSync(parent, {mode: 0o700}); info = lstatSync(parent); }
    assert.ok(info.isDirectory() && !info.isSymbolicLink(), 'Config parent must be a physical directory');
    if (process.getuid) assert.ok([0, process.getuid()].includes(info.uid) && (!(info.mode & 0o022) || info.mode & 0o1000), 'Config parent is not owner-controlled');
  }
  const pem = readFileSync(new URL('../keys/official-release-ed25519.pub.pem', import.meta.url));
  const der = createPublicKey(pem).export({type:'spki',format:'der'});
  const fingerprint = createHash('sha256').update(der).digest('hex');
  const authority = JSON.parse(readFileSync(new URL('../keys/official-release-authority.json', import.meta.url)));
  assert.equal(fingerprint, authority.spki_der_sha256, 'Shipped authority checksum mismatch');
  const path = join(directory, 'release-signing-key.pub.pem');
  let info;
  try { info = lstatSync(path); } catch (error) { if (error.code !== 'ENOENT') throw error; }
  if (info) {
    assert.ok(info.isFile() && !info.isSymbolicLink() && info.size <= 65536 && !(info.mode & 0o022), 'Existing key is unsafe; preserved without replacement');
    if (process.getuid) assert.ok([0, process.getuid()].includes(info.uid));
    let current;
    try { current = createPublicKey(readFileSync(path)).export({type:'spki',format:'der'}); }
    catch { throw new Error('Existing key is invalid; preserved without replacement'); }
    assert.ok(current.equals(der), 'Existing different authority preserved; explicit owner migration required');
    return {changed:false, path, spki_der_sha256:fingerprint};
  }
  writeFileSync(path, pem, {flag:'wx',mode:0o600});
  return {changed:true, path, spki_der_sha256:fingerprint};
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  if (args.length === 1 && args[0] === '--help') console.log('node scripts/install-release-trust.mjs [--config-dir ABS]\nInstalls only the shipped official public key. Preserves existing keys; no network/update/restart.');
  else {
    assert.ok(args.length === 0 || args.length === 2 && args[0] === '--config-dir');
    console.log(JSON.stringify(installReleaseTrust(args[1] || join(homedir(), '.config', 'treer'))));
  }
}
