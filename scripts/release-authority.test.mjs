import assert from 'node:assert/strict';
import test from 'node:test';
import { createHash, createPublicKey, generateKeyPairSync, verify } from 'node:crypto';
import { chmodSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { installReleaseTrust } from './install-release-trust.mjs';
import { keychainSigner } from './release-keychain.mjs';

function fixture(t) {
  const root = realpathSync.native(mkdtempSync(join(tmpdir(), 'treer-authority-')));
  t.after(() => rmSync(root, {recursive:true,force:true})); return root;
}
test('published public authority matches independent DER fingerprint and durable-custody signing proof', () => {
  const key = createPublicKey(readFileSync(new URL('../keys/official-release-ed25519.pub.pem', import.meta.url)));
  const metadata = JSON.parse(readFileSync(new URL('../keys/official-release-authority.json', import.meta.url)));
  const proof = JSON.parse(readFileSync(new URL('../keys/official-release-authority-proof.json', import.meta.url)));
  assert.equal(createHash('sha256').update(key.export({type:'spki',format:'der'})).digest('hex'), metadata.spki_der_sha256);
  const signature = Buffer.from(proof.signature_base64, 'base64');
  assert.ok(verify(null, Buffer.from(proof.message), key, signature));
  assert.equal(verify(null, Buffer.from(proof.message + 'changed'), key, signature), false);
});
test('legacy migration installs the shipped public key once and keeps an identical installation unchanged', t => {
  const root = fixture(t), directory = join(root, 'config');
  assert.equal(installReleaseTrust(directory).changed, true);
  const before = readFileSync(join(directory, 'release-signing-key.pub.pem'));
  assert.equal(installReleaseTrust(directory).changed, false);
  assert.deepEqual(readFileSync(join(directory, 'release-signing-key.pub.pem')), before);
});
test('legacy migration preserves invalid and different configured keys', t => {
  for (const data of [Buffer.from('invalid'), generateKeyPairSync('ed25519').publicKey.export({type:'spki',format:'pem'})]) {
    const root = fixture(t), path = join(root, 'release-signing-key.pub.pem');
    writeFileSync(path, data, {mode:0o600});
    assert.throws(() => installReleaseTrust(root));
    assert.deepEqual(readFileSync(path), Buffer.from(data));
  }
});
test('legacy migration refuses symlink keys and parent directories', t => {
  const root = fixture(t), target = join(root, 'target'); writeFileSync(target, 'retained');
  symlinkSync(target, join(root, 'release-signing-key.pub.pem'));
  assert.throws(() => installReleaseTrust(root));
  assert.equal(readFileSync(target, 'utf8'), 'retained');
  const link = join(root, 'config-link'); symlinkSync(root, link);
  assert.throws(() => installReleaseTrust(link));
});
test('publisher refuses a substituted signer before executing it', t => {
  if (!process.getuid) return t.skip('macOS publisher owner check');
  const root = fixture(t), path = join(root, 'signer');
  writeFileSync(path, '#!/bin/sh\nexit 99\n', {mode:0o700});
  assert.throws(() => keychainSigner(path, '0'.repeat(64)), /checksum/);
  chmodSync(path, 0o722);
  assert.throws(() => keychainSigner(path, '0'.repeat(64)), /owner/);
});
test('publisher rejects wrong authority and malformed signing results without a file fallback', t => {
  if (!process.getuid) return t.skip('macOS publisher owner check');
  const root = fixture(t), path = join(root, 'signer');
  const pem = readFileSync(new URL('../keys/official-release-ed25519.pub.pem', import.meta.url), 'utf8');
  const metadata = JSON.parse(readFileSync(new URL('../keys/official-release-authority.json', import.meta.url)));
  for (const wrong of [true, false]) {
    const record = {public_pem:pem, spki_der_sha256:wrong ? '0'.repeat(64) : metadata.spki_der_sha256};
    writeFileSync(path, `#!${process.execPath}\nconsole.log(JSON.stringify(process.argv[2]==='public'?${JSON.stringify(record)}:{schema_version:1,algorithm:'ed25519',key_id:'${metadata.key_id}',signature:'invalid'}));\n`, {mode:0o700});
    const hash = createHash('sha256').update(readFileSync(path)).digest('hex');
    if (wrong) assert.throws(() => keychainSigner(path, hash), /authority/);
    else assert.throws(() => keychainSigner(path, hash).signDetached(Buffer.from('message')), /signature/);
  }
});
