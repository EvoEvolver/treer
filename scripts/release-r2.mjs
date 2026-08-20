#!/usr/bin/env node

import {
  chmodSync,
  existsSync,
  mkdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, sign, verify } from "node:crypto";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { execFileSync } from "node:child_process";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const DEFAULT_BUCKET = "treer-releases";
const DEFAULT_BASE_URL = "https://releases.treer.ai/";
const DEFAULT_PLATFORMS = [
  "linux-x86_64",
  "linux-aarch64",
  "darwin-x86_64",
  "darwin-aarch64",
];
const BINARIES = ["treer", "treer-agent-server", "treer-agent-host"];
const MAX_ARTIFACT_BYTES = 128 * 1024 * 1024;
const IMMUTABLE_CACHE_CONTROL = "public, max-age=31536000, immutable";
const CHANNEL_CACHE_CONTROL = "no-store";

function fail(message) {
  throw new Error(message);
}

export function normalizeVersion(value) {
  if (!/^v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$/.test(value ?? "")) {
    fail(`invalid release version ${JSON.stringify(value)}; expected vMAJOR.MINOR.PATCH[-PRERELEASE]`);
  }
  return value;
}

function normalizeChannel(value) {
  if (!/^[a-z][a-z0-9-]*$/.test(value ?? "")) {
    fail(`invalid channel ${JSON.stringify(value)}`);
  }
  return value;
}

function normalizeBaseUrl(value) {
  const url = new URL(value);
  if (url.protocol !== "https:") {
    fail("release base URL must use HTTPS");
  }
  if (!url.pathname.endsWith("/")) {
    url.pathname += "/";
  }
  url.search = "";
  url.hash = "";
  return url;
}

function defaultPrivateKeyPath() {
  return join(homedir(), ".config", "treer", "release-signing-key.pem");
}

function defaultPublicKeyPath(privateKeyPath) {
  return privateKeyPath.endsWith(".pem")
    ? `${privateKeyPath.slice(0, -4)}.pub.pem`
    : `${privateKeyPath}.pub.pem`;
}

function sha256Bytes(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function sha256File(path) {
  const hash = createHash("sha256");
  const file = readFileSync(path);
  hash.update(file);
  return hash.digest("hex");
}

function stableJson(value) {
  return Buffer.from(`${JSON.stringify(value, null, 2)}\n`, "utf8");
}

function publicKeyId(publicKey) {
  const der = publicKey.export({ type: "spki", format: "der" });
  return sha256Bytes(der).slice(0, 16);
}

function detachedSignature(bytes, privateKey) {
  const publicKey = createPublicKey(privateKey);
  return {
    schema_version: 1,
    algorithm: "ed25519",
    key_id: publicKeyId(publicKey),
    signature: sign(null, bytes, privateKey).toString("base64"),
  };
}

export function verifyDetachedSignature(bytes, signature, publicKey) {
  if (signature?.schema_version !== 1 || signature?.algorithm !== "ed25519") {
    fail("unsupported detached signature format");
  }
  if (signature.key_id !== publicKeyId(publicKey)) {
    fail(`signature key ${signature.key_id} does not match the configured release key`);
  }
  const raw = Buffer.from(signature.signature ?? "", "base64");
  if (!verify(null, bytes, publicKey, raw)) {
    fail("release signature verification failed");
  }
}

export function generateReleaseKey(privateKeyPath, publicKeyPath = defaultPublicKeyPath(privateKeyPath)) {
  if (existsSync(privateKeyPath) || existsSync(publicKeyPath)) {
    fail(`refusing to overwrite an existing release key at ${privateKeyPath} or ${publicKeyPath}`);
  }
  mkdirSync(dirname(privateKeyPath), { recursive: true, mode: 0o700 });
  mkdirSync(dirname(publicKeyPath), { recursive: true, mode: 0o700 });
  const { privateKey, publicKey } = generateKeyPairSync("ed25519", {
    privateKeyEncoding: { type: "pkcs8", format: "pem" },
    publicKeyEncoding: { type: "spki", format: "pem" },
  });
  writeFileSync(privateKeyPath, privateKey, { mode: 0o600, flag: "wx" });
  writeFileSync(publicKeyPath, publicKey, { mode: 0o644, flag: "wx" });
  chmodSync(privateKeyPath, 0o600);
  return publicKeyId(createPublicKey(publicKey));
}

function workspaceVersion() {
  const cargo = readFileSync(join(root, "Cargo.toml"), "utf8");
  const section = cargo.match(/\[workspace\.package\]([\s\S]*?)(?:\n\[|$)/)?.[1] ?? "";
  const version = section.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version) {
    fail("could not read workspace.package.version from Cargo.toml");
  }
  return version;
}

function requireMatchingWorkspaceVersion(version) {
  const packageVersion = workspaceVersion();
  const releaseVersion = version.slice(1);
  if (releaseVersion !== packageVersion && !releaseVersion.startsWith(`${packageVersion}-`)) {
    fail(`release ${version} does not match workspace version ${packageVersion}`);
  }
}

function gitOutput(args) {
  return execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();
}

function requireCleanGit() {
  const status = gitOutput(["status", "--porcelain"]);
  if (status) {
    fail("refusing to release from a dirty worktree");
  }
}

function currentCommit() {
  return gitOutput(["rev-parse", "HEAD"]);
}

function requireStableTag(version, expectedCommit) {
  if (version.includes("-")) {
    fail(`stable releases cannot use a prerelease version: ${version}`);
  }
  let taggedCommit;
  try {
    taggedCommit = gitOutput(["rev-list", "-n", "1", version]);
  } catch {
    fail(`stable promotion requires the local ${version} tag`);
  }
  if (taggedCommit !== expectedCommit) {
    fail(`tag ${version} points to ${taggedCommit}, but the release was built from ${expectedCommit}`);
  }
}

export function buildArtifactRecords({ distDir, platforms, version }) {
  const records = [];
  for (const platform of platforms) {
    if (!DEFAULT_PLATFORMS.includes(platform)) {
      fail(`unsupported release platform ${platform}`);
    }
    for (const binary of BINARIES) {
      const source = join(distDir, platform, binary);
      if (!existsSync(source) || !statSync(source).isFile()) {
        fail(`missing release artifact ${source}`);
      }
      const size = statSync(source).size;
      if (size === 0) {
        fail(`release artifact ${source} is empty`);
      }
      if (size > MAX_ARTIFACT_BYTES) {
        fail(`release artifact ${source} exceeds the 128 MiB updater limit`);
      }
      if ((statSync(source).mode & 0o111) === 0) {
        fail(`release artifact ${source} is not executable`);
      }
      records.push({
        binary,
        platform,
        path: `releases/${version}/${platform}/${binary}`,
        sha256: sha256File(source),
        size,
        source,
      });
    }
  }
  return records.sort((left, right) =>
    left.platform.localeCompare(right.platform) || left.binary.localeCompare(right.binary),
  );
}

function comparableArtifacts(records) {
  return records.map(({ source: _source, ...record }) => record);
}

function releaseOutputDir(version, override) {
  return resolve(override ?? join(root, "target", "releases", version));
}

function loadPrivateKey(path) {
  if (!existsSync(path)) {
    fail(`release signing key not found at ${path}; run just artifacts-keygen first`);
  }
  const mode = statSync(path).mode & 0o777;
  if ((mode & 0o077) !== 0) {
    fail(`release signing key ${path} must not be readable by group or other users`);
  }
  return createPrivateKey(readFileSync(path));
}

function loadPublicKey(publicKeyPath, privateKeyPath) {
  if (existsSync(publicKeyPath)) {
    return createPublicKey(readFileSync(publicKeyPath));
  }
  return createPublicKey(loadPrivateKey(privateKeyPath));
}

function readDetached(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

export function prepareRelease({
  version,
  distDir,
  outputDir,
  platforms,
  privateKeyPath,
  gitCommit,
  createdAt = new Date().toISOString(),
}) {
  const privateKey = loadPrivateKey(privateKeyPath);
  const artifacts = buildArtifactRecords({ distDir, platforms, version });
  mkdirSync(outputDir, { recursive: true });
  const manifestPath = join(outputDir, "manifest.json");
  const signaturePath = join(outputDir, "manifest.sig");

  if (existsSync(manifestPath) || existsSync(signaturePath)) {
    if (!existsSync(manifestPath) || !existsSync(signaturePath)) {
      fail(`incomplete prepared release in ${outputDir}`);
    }
    const bytes = readFileSync(manifestPath);
    const manifest = JSON.parse(bytes);
    verifyDetachedSignature(bytes, readDetached(signaturePath), createPublicKey(privateKey));
    if (
      manifest.version !== version ||
      manifest.git_commit !== gitCommit ||
      JSON.stringify(manifest.artifacts) !== JSON.stringify(comparableArtifacts(artifacts))
    ) {
      fail(`prepared release ${outputDir} does not match the current commit or artifacts`);
    }
    return { artifacts, manifest, manifestBytes: bytes, manifestPath, signaturePath };
  }

  const publicKey = createPublicKey(privateKey);
  const manifest = {
    schema_version: 1,
    version,
    git_commit: gitCommit,
    created_at: createdAt,
    signing_key_id: publicKeyId(publicKey),
    artifacts: comparableArtifacts(artifacts),
  };
  const manifestBytes = stableJson(manifest);
  writeFileSync(manifestPath, manifestBytes, { flag: "wx" });
  writeFileSync(signaturePath, stableJson(detachedSignature(manifestBytes, privateKey)), { flag: "wx" });
  verifyDetachedSignature(manifestBytes, readDetached(signaturePath), publicKey);
  return { artifacts, manifest, manifestBytes, manifestPath, signaturePath };
}

function wranglerArgs(profile, args) {
  return profile ? ["--profile", profile, ...args] : args;
}

function uploadObject({ wrangler, profile, bucket, key, file, contentType, cacheControl }) {
  const args = wranglerArgs(profile, [
    "r2",
    "object",
    "put",
    `${bucket}/${key}`,
    "--remote",
    "--file",
    file,
    "--content-type",
    contentType,
    "--cache-control",
    cacheControl,
  ]);
  execFileSync(wrangler, args, { cwd: root, stdio: "inherit" });
}

async function fetchExact(url, expectedBytes) {
  const response = await fetch(url, { headers: { "cache-control": "no-cache" } });
  if (!response.ok) {
    return { status: response.status, equal: false, bytes: null };
  }
  const bytes = Buffer.from(await response.arrayBuffer());
  return { status: response.status, equal: bytes.equals(expectedBytes), bytes };
}

async function fetchJsonWithSignature(baseUrl, path, publicKey) {
  const [payloadResponse, signatureResponse] = await Promise.all([
    fetch(new URL(path, baseUrl), { headers: { "cache-control": "no-cache" } }),
    fetch(new URL(`${path}.sig`, baseUrl), { headers: { "cache-control": "no-cache" } }),
  ]);
  if (!payloadResponse.ok || !signatureResponse.ok) {
    fail(`failed to fetch signed ${path}: HTTP ${payloadResponse.status}/${signatureResponse.status}`);
  }
  const bytes = Buffer.from(await payloadResponse.arrayBuffer());
  const signature = JSON.parse(await signatureResponse.text());
  verifyDetachedSignature(bytes, signature, publicKey);
  return { bytes, value: JSON.parse(bytes) };
}

async function verifyRemoteArtifact(baseUrl, artifact) {
  const url = new URL(artifact.path, baseUrl);
  const response = await fetch(url, { headers: { "cache-control": "no-cache" } });
  if (!response.ok) {
    fail(`failed to download ${url}: HTTP ${response.status}`);
  }
  const hash = createHash("sha256");
  let size = 0;
  for await (const chunk of response.body) {
    hash.update(chunk);
    size += chunk.byteLength;
  }
  if (size !== artifact.size) {
    fail(`remote ${artifact.path} has size ${size}, expected ${artifact.size}`);
  }
  const digest = hash.digest("hex");
  if (digest !== artifact.sha256) {
    fail(`remote ${artifact.path} has SHA-256 ${digest}, expected ${artifact.sha256}`);
  }
}

export async function verifyRemoteRelease({ baseUrl, version, publicKey }) {
  const signed = await fetchJsonWithSignature(baseUrl, `releases/${version}/manifest.json`, publicKey);
  if (signed.value.schema_version !== 1 || signed.value.version !== version) {
    fail(`remote manifest does not describe ${version}`);
  }
  if (signed.value.signing_key_id !== publicKeyId(publicKey)) {
    fail(`remote manifest names unexpected signing key ${signed.value.signing_key_id}`);
  }
  for (const artifact of signed.value.artifacts) {
    await verifyRemoteArtifact(baseUrl, artifact);
  }
  return signed.value;
}

function channelPayload({ channel, version, manifestBytes, updatedAt = new Date().toISOString() }) {
  return {
    schema_version: 1,
    channel,
    version,
    manifest_path: `releases/${version}/manifest.json`,
    manifest_sha256: sha256Bytes(manifestBytes),
    updated_at: updatedAt,
  };
}

async function uploadChannel({
  channel,
  version,
  manifestBytes,
  privateKey,
  outputDir,
  wrangler,
  profile,
  bucket,
  baseUrl,
}) {
  const payload = channelPayload({ channel, version, manifestBytes });
  const bytes = stableJson(payload);
  const channelPath = join(outputDir, `${channel}.json`);
  const signaturePath = join(outputDir, `${channel}.json.sig`);
  writeFileSync(channelPath, bytes);
  writeFileSync(signaturePath, stableJson(detachedSignature(bytes, privateKey)));

  // Upload the signature first. Readers may briefly reject the old channel,
  // but they never accept a new pointer with an old signature.
  uploadObject({
    wrangler,
    profile,
    bucket,
    key: `channels/${channel}.json.sig`,
    file: signaturePath,
    contentType: "application/json",
    cacheControl: CHANNEL_CACHE_CONTROL,
  });
  uploadObject({
    wrangler,
    profile,
    bucket,
    key: `channels/${channel}.json`,
    file: channelPath,
    contentType: "application/json",
    cacheControl: CHANNEL_CACHE_CONTROL,
  });
  const remote = await fetchJsonWithSignature(baseUrl, `channels/${channel}.json`, createPublicKey(privateKey));
  if (remote.value.version !== version || remote.value.manifest_sha256 !== payload.manifest_sha256) {
    fail(`remote ${channel} channel did not converge to ${version}`);
  }
}

async function publishRelease(config) {
  requireCleanGit();
  requireMatchingWorkspaceVersion(config.version);
  const gitCommit = currentCommit();
  const prepared = prepareRelease({ ...config, gitCommit });
  const manifestUrl = new URL(`releases/${config.version}/manifest.json`, config.baseUrl);
  const existing = await fetchExact(manifestUrl, prepared.manifestBytes);
  if (existing.status !== 200 && existing.status !== 404) {
    fail(`failed to check immutable release ${config.version}: HTTP ${existing.status}`);
  }
  if (existing.status !== 404 && !existing.equal) {
    fail(`immutable release ${config.version} already exists with different contents`);
  }

  if (existing.status === 404) {
    for (const artifact of prepared.artifacts) {
      uploadObject({
        ...config,
        key: artifact.path,
        file: artifact.source,
        contentType: "application/octet-stream",
        cacheControl: IMMUTABLE_CACHE_CONTROL,
      });
    }
    uploadObject({
      ...config,
      key: `releases/${config.version}/manifest.json.sig`,
      file: prepared.signaturePath,
      contentType: "application/json",
      cacheControl: IMMUTABLE_CACHE_CONTROL,
    });
    uploadObject({
      ...config,
      key: `releases/${config.version}/manifest.json`,
      file: prepared.manifestPath,
      contentType: "application/json",
      cacheControl: IMMUTABLE_CACHE_CONTROL,
    });
  }

  const privateKey = loadPrivateKey(config.privateKeyPath);
  await verifyRemoteRelease({ baseUrl: config.baseUrl, version: config.version, publicKey: createPublicKey(privateKey) });
  await uploadChannel({
    ...config,
    channel: config.channel,
    manifestBytes: prepared.manifestBytes,
    privateKey,
  });
  console.log(`published ${config.version} to the ${config.channel} channel`);
}

async function promoteRelease(config) {
  requireCleanGit();
  const privateKey = loadPrivateKey(config.privateKeyPath);
  const publicKey = createPublicKey(privateKey);
  const source = await fetchJsonWithSignature(config.baseUrl, `channels/${config.fromChannel}.json`, publicKey);
  if (source.value.version !== config.version) {
    fail(`${config.fromChannel} currently points to ${source.value.version}, not ${config.version}`);
  }
  const manifest = await verifyRemoteRelease({ baseUrl: config.baseUrl, version: config.version, publicKey });
  const manifestResponse = await fetch(new URL(manifestPath(config.version), config.baseUrl));
  if (!manifestResponse.ok) {
    fail(`failed to fetch ${config.version} manifest: HTTP ${manifestResponse.status}`);
  }
  const manifestBytes = Buffer.from(await manifestResponse.arrayBuffer());
  if (sha256Bytes(manifestBytes) !== source.value.manifest_sha256) {
    fail(`${config.fromChannel} manifest digest does not match ${config.version}`);
  }
  if (config.channel === "stable") {
    requireStableTag(config.version, manifest.git_commit);
  }
  await uploadChannel({ ...config, manifestBytes, privateKey });
  console.log(`promoted ${config.version} from ${config.fromChannel} to ${config.channel}`);
}

function manifestPath(version) {
  return `releases/${version}/manifest.json`;
}

function optionValue(options, name, environment, fallback) {
  return options[name] ?? process.env[environment] ?? fallback;
}

function parsePlatforms(options) {
  const fromArguments = options.platform;
  const fromEnvironment = process.env.TREER_RELEASE_PLATFORMS?.split(",").map((value) => value.trim()).filter(Boolean);
  return fromArguments?.length ? fromArguments : fromEnvironment?.length ? fromEnvironment : DEFAULT_PLATFORMS;
}

function commonConfig(options) {
  const privateKeyPath = resolve(optionValue(options, "private-key", "TREER_RELEASE_SIGNING_KEY", defaultPrivateKeyPath()));
  const version = options.version ? normalizeVersion(options.version) : undefined;
  return {
    version,
    distDir: resolve(options.dist ?? join(root, "dist")),
    outputDir: version ? releaseOutputDir(version, options.output) : undefined,
    platforms: parsePlatforms(options),
    privateKeyPath,
    publicKeyPath: resolve(optionValue(options, "public-key", "TREER_RELEASE_PUBLIC_KEY", defaultPublicKeyPath(privateKeyPath))),
    bucket: optionValue(options, "bucket", "TREER_RELEASE_BUCKET", DEFAULT_BUCKET),
    baseUrl: normalizeBaseUrl(optionValue(options, "base-url", "TREER_RELEASE_BASE_URL", DEFAULT_BASE_URL)),
    wrangler: optionValue(options, "wrangler", "TREER_WRANGLER", "wrangler"),
    profile: optionValue(options, "profile", "TREER_CLOUDFLARE_PROFILE", undefined),
  };
}

function usage() {
  console.log(`Usage:
  node scripts/release-r2.mjs keygen
  node scripts/release-r2.mjs prepare --version vMAJOR.MINOR.PATCH
  node scripts/release-r2.mjs publish --version vMAJOR.MINOR.PATCH --channel canary
  node scripts/release-r2.mjs promote --version vMAJOR.MINOR.PATCH --from-channel canary --channel stable
  node scripts/release-r2.mjs verify --version vMAJOR.MINOR.PATCH

Environment:
  TREER_RELEASE_SIGNING_KEY       PKCS#8 Ed25519 private key
  TREER_RELEASE_PUBLIC_KEY        SPKI Ed25519 public key
  TREER_RELEASE_BUCKET            R2 bucket (default: ${DEFAULT_BUCKET})
  TREER_RELEASE_BASE_URL          HTTPS custom domain (default: ${DEFAULT_BASE_URL})
  TREER_RELEASE_PLATFORMS         Comma-separated platform list
  TREER_CLOUDFLARE_PROFILE        Optional Wrangler auth profile`);
}

async function main() {
  const { values: options, positionals } = parseArgs({
    allowPositionals: true,
    options: {
      version: { type: "string" },
      channel: { type: "string" },
      "from-channel": { type: "string" },
      dist: { type: "string" },
      output: { type: "string" },
      platform: { type: "string", multiple: true },
      "private-key": { type: "string" },
      "public-key": { type: "string" },
      bucket: { type: "string" },
      "base-url": { type: "string" },
      wrangler: { type: "string" },
      profile: { type: "string" },
      help: { type: "boolean", short: "h" },
    },
  });
  const command = positionals[0];
  if (options.help || !command) {
    usage();
    return;
  }
  const config = commonConfig(options);

  if (command === "keygen") {
    const keyId = generateReleaseKey(config.privateKeyPath, config.publicKeyPath);
    console.log(`created Ed25519 release key ${keyId}`);
    console.log(`private: ${config.privateKeyPath}`);
    console.log(`public:  ${config.publicKeyPath}`);
    return;
  }
  if (!config.version) {
    fail(`${command} requires --version`);
  }
  if (command === "prepare") {
    requireCleanGit();
    requireMatchingWorkspaceVersion(config.version);
    const prepared = prepareRelease({ ...config, gitCommit: currentCommit() });
    console.log(`prepared ${config.version} in ${dirname(prepared.manifestPath)}`);
    return;
  }
  if (command === "publish") {
    config.channel = normalizeChannel(options.channel ?? "canary");
    if (config.channel === "stable") {
      fail("publish to canary first, then use promote for stable");
    }
    await publishRelease(config);
    return;
  }
  if (command === "promote") {
    config.channel = normalizeChannel(options.channel ?? "stable");
    config.fromChannel = normalizeChannel(options["from-channel"] ?? "canary");
    await promoteRelease(config);
    return;
  }
  if (command === "verify") {
    const publicKey = loadPublicKey(config.publicKeyPath, config.privateKeyPath);
    await verifyRemoteRelease({ baseUrl: config.baseUrl, version: config.version, publicKey });
    console.log(`verified ${config.version} from ${config.baseUrl}`);
    return;
  }
  fail(`unknown command ${command}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(`release failed: ${error.message}`);
    process.exitCode = 1;
  });
}
