import assert from 'node:assert/strict';
import { createHash, createPublicKey, verify } from 'node:crypto';
import { lstatSync, readFileSync, realpathSync } from 'node:fs';
import { isAbsolute } from 'node:path';
import { execFileSync } from 'node:child_process';

export function keychainSigner(executable, sha256) {
  assert.ok(isAbsolute(executable) && realpathSync.native(executable) === executable, 'Signer must be a physical absolute path');
  const info = lstatSync(executable);
  assert.ok(info.isFile() && !info.isSymbolicLink() && !(info.mode & 0o022) && info.uid === process.getuid(), 'Signer must be owner-controlled');
  assert.match(sha256 || '', /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(executable)).digest('hex'), sha256, 'Signer checksum mismatch');
  const pem = readFileSync(new URL('../keys/official-release-ed25519.pub.pem', import.meta.url));
  const publicKey = createPublicKey(pem);
  const metadata = JSON.parse(readFileSync(new URL('../keys/official-release-authority.json', import.meta.url)));
  const fingerprint = createHash('sha256').update(publicKey.export({ type: 'spki', format: 'der' })).digest('hex');
  assert.equal(fingerprint, metadata.spki_der_sha256);
  function call(mode, input) {
    try {
      return JSON.parse(execFileSync(executable, [mode], { input, timeout: 10000, maxBuffer: 8192, stdio: ['pipe', 'pipe', 'ignore'] }));
    } catch { throw new Error('Keychain publisher refused; no file-key fallback or retry'); }
  }
  const record = call('public');
  assert.equal(record.spki_der_sha256, fingerprint, 'Keychain authority mismatch');
  assert.ok(createPublicKey(record.public_pem).export({ type: 'spki', format: 'der' }).equals(publicKey.export({ type: 'spki', format: 'der' })));
  return {
    publisherPublicKey: publicKey,
    signDetached(bytes) {
      assert.ok(bytes.length <= 1024 * 1024, 'Signing metadata exceeds limit');
      const signed = call('sign', bytes);
      assert.deepEqual(Object.keys(signed).sort(), ['algorithm', 'key_id', 'schema_version', 'signature']);
      assert.equal(signed.schema_version, 1); assert.equal(signed.algorithm, 'ed25519');
      assert.equal(signed.key_id, fingerprint.slice(0, 16));
      assert.equal(typeof signed.signature, 'string');
      const signature = Buffer.from(signed.signature, 'base64');
      assert.ok(signature.length === 64 && signature.toString('base64') === signed.signature && verify(null, bytes, publicKey, signature), 'Keychain signature mismatch');
      return signature;
    },
  };
}
