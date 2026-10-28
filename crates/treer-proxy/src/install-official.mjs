// Embedded verbatim in the official Unix bootstrap; never execute downloaded code.
import { createHash, createPublicKey, verify } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';

const BASE = 'https://releases.treer.ai/';
const PEM = `__TREER_OFFICIAL_RELEASE_PEM__`;
const FINGERPRINT = '634761ff47a6a113b4b8925f0db0e030d8bfce07f2fa228fdc0a4904874dc41f';
const names = ['treer', 'treer-launch', 'treer-agent-host', 'treer-agent-server'];
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const requireThat = (condition, code) => { if (!condition) throw new Error(code); };

export async function downloadOfficial(platform, directory, { transport = fetch } = {}) {
  requireThat(['linux-x86_64', 'linux-aarch64', 'darwin-x86_64', 'darwin-aarch64'].includes(platform), 'unsupported_platform');
  const key = createPublicKey(PEM);
  requireThat(key.asymmetricKeyType === 'ed25519', 'invalid_key_type');
  // The production entry has no public-key/origin/channel environment override.
  const fingerprint = hash(key.export({type: 'spki', format: 'der'}));
  requireThat(fingerprint === FINGERPRINT, 'official_key_mismatch');
  const keyId = fingerprint.slice(0, 16);
  async function get(path, bound) {
    requireThat(!path.includes('..') && /^[A-Za-z0-9/_.-]+$/.test(path), 'invalid_release_path');
    const url = new URL(path, BASE);
    requireThat(url.origin === new URL(BASE).origin, 'release_origin_mismatch');
    const signal = AbortSignal.timeout(120000);
    const response = await transport(url, {redirect: 'error', signal, headers: {'cache-control': 'no-cache'}});
    requireThat(response.status === 200, `release_http_${response.status}`);
    const length = response.headers.get('content-length');
    if (length !== null) requireThat(/^\d+$/.test(length) && Number(length) <= bound, 'release_oversized');
    const chunks = []; let size = 0;
    try {
      for await (const chunk of response.body) {
        size += chunk.length;
        requireThat(size <= bound, 'release_oversized');
        chunks.push(chunk);
      }
      return Buffer.concat(chunks);
    } finally { if (!response.body?.locked) await response.body?.cancel().catch(() => {}); }
  }
  async function signed(path, expectedHash) {
    const bytes = await get(path, 1024 * 1024);
    if (expectedHash) requireThat(hash(bytes) === expectedHash, 'manifest_hash_mismatch');
    const signature = JSON.parse(await get(path + '.sig', 4096));
    requireThat(signature.schema_version === 1 && signature.algorithm === 'ed25519' && signature.key_id === keyId, 'invalid_signature_metadata');
    const raw = Buffer.from(signature.signature ?? '', 'base64');
    requireThat(raw.length === 64 && raw.toString('base64') === signature.signature && verify(null, bytes, key, raw), 'release_signature_invalid');
    return JSON.parse(bytes);
  }
  const channel = await signed('channels/stable.json');
  requireThat(channel.schema_version === 1 && channel.channel === 'stable' && /^v\d+\.\d+\.\d+$/.test(channel.version), 'invalid_stable_channel');
  requireThat(channel.manifest_path === `releases/${channel.version}/manifest.json` && /^[a-f0-9]{64}$/.test(channel.manifest_sha256), 'invalid_manifest_binding');
  const manifest = await signed(channel.manifest_path, channel.manifest_sha256);
  requireThat(manifest.schema_version === 2 && manifest.version === channel.version && manifest.signing_key_id === keyId && /^[a-f0-9]{40}$/.test(manifest.git_commit) && Array.isArray(manifest.artifacts), 'invalid_manifest');
  const rows = names.map(binary => {
    const matches = manifest.artifacts.filter(row => row.platform === platform && row.binary === binary);
    requireThat(matches.length === 1, 'missing_or_duplicate_artifact');
    const row = matches[0];
    requireThat(row.path === `releases/${channel.version}/${platform}/${binary}` && Number.isSafeInteger(row.size) && row.size > 0 && row.size <= 128 * 1024 * 1024 && /^[a-f0-9]{64}$/.test(row.sha256), 'invalid_artifact_binding');
    return row;
  });
  // Stage and verify the entire set before the surrounding shell touches installation paths.
  for (const row of rows) {
    const bytes = await get(row.path, row.size);
    requireThat(bytes.length === row.size && hash(bytes) === row.sha256, 'artifact_hash_or_size_mismatch');
    writeFileSync(join(directory, row.binary), bytes, {flag: 'wx', mode: 0o600});
  }
  return {version: manifest.version, source: manifest.git_commit, platform};
}

if (process.argv[1] === '-') {
  try {
    requireThat(Number(process.versions.node.split('.')[0]) >= 22, 'Node.js_22_or_newer_required');
    const result = await downloadOfficial(process.argv[2], process.argv[3]);
    console.log(`treer: verified official stable ${result.version} ${result.source} ${result.platform}`);
  } catch (error) {
    console.error(`treer: official signed bootstrap refused: ${error.message}`);
    process.exitCode = 1;
  }
}
