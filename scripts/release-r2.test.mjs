import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { createPublicKey } from "node:crypto";

import {
  buildArtifactRecords,
  buildProvenanceRecords,
  generateReleaseKey,
  normalizeVersion,
  prepareRelease,
  readR2Object,
  verifyDetachedSignature,
} from "./release-r2.mjs";

function fixture(gitCommit = "a".repeat(40), version = "1.2.3") {
  const directory = mkdtempSync(join(tmpdir(), "treer-release-test-"));
  const distDir = join(directory, "dist");
  const platform = "linux-x86_64";
  mkdirSync(join(distDir, platform), { recursive: true });
  for (const binary of ["treer", "treer-agent-server", "treer-agent-host", "treer-launch"]) {
    const path = join(distDir, platform, binary);
    writeFileSync(path, `fixture ${binary}\n`);
    chmodSync(path, 0o755);
  }
  writeFileSync(join(distDir, platform, "build-metadata.json"), `${JSON.stringify({
    schema_version: 1,
    git_commit: gitCommit,
    version,
    platform,
    rustc: "rustc 1.90.0 (fixture)",
  }, null, 2)}\n`);
  const privateKeyPath = join(directory, "release-key.pem");
  const publicKeyPath = join(directory, "release-key.pub.pem");
  generateReleaseKey(privateKeyPath, publicKeyPath);
  return {
    directory,
    distDir,
    platform,
    privateKeyPath,
    publicKeyPath,
    outputDir: join(directory, "out"),
  };
}

test("release versions reject paths and mutable aliases", () => {
  assert.equal(normalizeVersion("v1.2.3"), "v1.2.3");
  assert.equal(normalizeVersion("v1.2.3-canary.1"), "v1.2.3-canary.1");
  for (const invalid of ["latest", "1.2.3", "v1.2", "v1.2.3/other", "v01.2.3"]) {
    assert.throws(() => normalizeVersion(invalid));
  }
});

test("Windows artifacts use logical manifest names and require PE files", (t) => {
  const data = fixture();
  t.after(() => rmSync(data.directory, { recursive: true, force: true }));
  const platform = "windows-x86_64";
  mkdirSync(join(data.distDir, platform));
  for (const binary of ["treer", "treer-agent-server", "treer-agent-host", "treer-launch"]) {
    writeFileSync(join(data.distDir, platform, binary), `MZ fixture ${binary}`, { mode: 0o644 });
  }
  const options = { distDir: data.distDir, platforms: [platform], version: "v1.2.3" };
  const records = buildArtifactRecords(options);
  assert.equal(records.length, 4);
  assert.equal(records[0].path, "releases/v1.2.3/windows-x86_64/treer");
  writeFileSync(join(data.distDir, platform, "treer"), "ELF");
  assert.throws(() => buildArtifactRecords(options), /not a Windows executable/);
});

test("manifest contains sorted immutable artifact records and a valid signature", () => {
  const data = fixture();
  try {
    const prepared = prepareRelease({
      version: "v1.2.3-canary.1",
      distDir: data.distDir,
      outputDir: data.outputDir,
      platforms: [data.platform],
      privateKeyPath: data.privateKeyPath,
      gitCommit: "a".repeat(40),
      createdAt: "2026-08-20T00:00:00.000Z",
    });
    assert.equal(prepared.manifest.version, "v1.2.3-canary.1");
    assert.equal(prepared.manifest.schema_version, 2);
    assert.deepEqual(prepared.manifest.builds, [{
      platform: data.platform,
      version: "1.2.3",
      git_commit: "a".repeat(40),
      rustc: "rustc 1.90.0 (fixture)",
    }]);
    assert.equal(prepared.manifest.artifacts.length, 4);
    assert.deepEqual(
      prepared.manifest.artifacts.map((artifact) => artifact.binary),
      ["treer", "treer-agent-host", "treer-agent-server", "treer-launch"],
    );
    assert.ok(prepared.manifest.artifacts.every((artifact) => !Object.hasOwn(artifact, "source")));
    const signature = JSON.parse(readFileSync(prepared.signaturePath, "utf8"));
    const publicKey = createPublicKey(readFileSync(data.publicKeyPath));
    verifyDetachedSignature(prepared.manifestBytes, signature, publicKey);

    const reused = prepareRelease({
      version: "v1.2.3-canary.1",
      distDir: data.distDir,
      outputDir: data.outputDir,
      platforms: [data.platform],
      privateKeyPath: data.privateKeyPath,
      gitCommit: "a".repeat(40),
    });
    assert.deepEqual(reused.manifestBytes, prepared.manifestBytes);
  } finally {
    rmSync(data.directory, { recursive: true, force: true });
  }
});

test("prepared releases cannot be silently reused after an artifact changes", () => {
  const data = fixture("b".repeat(40));
  try {
    const options = {
      version: "v1.2.3",
      distDir: data.distDir,
      outputDir: data.outputDir,
      platforms: [data.platform],
      privateKeyPath: data.privateKeyPath,
      gitCommit: "b".repeat(40),
    };
    prepareRelease(options);
    writeFileSync(join(data.distDir, data.platform, "treer"), "changed\n");
    assert.throws(() => prepareRelease(options), /does not match/);
  } finally {
    rmSync(data.directory, { recursive: true, force: true });
  }
});

test("build provenance must match the release commit and platform", () => {
  const data = fixture();
  try {
    const records = buildProvenanceRecords({
      distDir: data.distDir,
      platforms: [data.platform],
      version: "v1.2.3",
      gitCommit: "a".repeat(40),
    });
    assert.equal(records[0].rustc, "rustc 1.90.0 (fixture)");
    assert.throws(
      () => buildProvenanceRecords({
        distDir: data.distDir,
        platforms: [data.platform],
        version: "v1.2.3",
        gitCommit: "b".repeat(40),
      }),
      /does not match/,
    );
  } finally {
    rmSync(data.directory, { recursive: true, force: true });
  }
});

test("all requested binaries are mandatory", () => {
  const directory = mkdtempSync(join(tmpdir(), "treer-release-test-"));
  try {
    mkdirSync(join(directory, "linux-x86_64"), { recursive: true });
    const path = join(directory, "linux-x86_64", "treer");
    writeFileSync(path, "treer\n");
    chmodSync(path, 0o755);
    assert.throws(
      () => buildArtifactRecords({ distDir: directory, platforms: ["linux-x86_64"], version: "v1.2.3" }),
      /missing release artifact/,
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

function fakeWrangler(body) {
  const directory = mkdtempSync(join(tmpdir(), "treer-fake-wrangler-"));
  const path = join(directory, "wrangler");
  writeFileSync(path, `#!/bin/sh\nprintf '%s\\n' "$@" > "${directory}/args"\n${body}\n`);
  chmodSync(path, 0o755);
  return { directory, path };
}

test("R2 existence checks read the bucket through Wrangler, not the public URL", (t) => {
  const present = fakeWrangler(`while [ "$1" != "--file" ]; do shift; done; printf 'manifest-bytes' > "$2"`);
  t.after(() => rmSync(present.directory, { recursive: true, force: true }));
  const bytes = readR2Object({ wrangler: present.path, bucket: "bucket", key: "releases/v1.2.3/manifest.json", profile: "ops" });
  assert.equal(bytes.toString(), "manifest-bytes");
  const args = readFileSync(join(present.directory, "args"), "utf8").trim().split("\n");
  assert.deepEqual(args.slice(0, 7), ["--profile", "ops", "r2", "object", "get", "bucket/releases/v1.2.3/manifest.json", "--remote"]);
  assert.ok(!args.some((arg) => arg.startsWith("http")));
});

test("missing R2 objects read as absent, other Wrangler failures stop the release", (t) => {
  const missing = fakeWrangler(`echo 'ERROR The specified key does not exist.' >&2; exit 1`);
  const broken = fakeWrangler(`echo 'ERROR Authentication error [code: 10000]' >&2; exit 1`);
  t.after(() => {
    rmSync(missing.directory, { recursive: true, force: true });
    rmSync(broken.directory, { recursive: true, force: true });
  });
  assert.equal(readR2Object({ wrangler: missing.path, bucket: "bucket", key: "k" }), null);
  assert.throws(() => readR2Object({ wrangler: broken.path, bucket: "bucket", key: "k" }), /Authentication error/);
});
