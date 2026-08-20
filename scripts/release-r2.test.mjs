import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { createPublicKey } from "node:crypto";

import {
  buildArtifactRecords,
  generateReleaseKey,
  normalizeVersion,
  prepareRelease,
  verifyDetachedSignature,
} from "./release-r2.mjs";

function fixture() {
  const directory = mkdtempSync(join(tmpdir(), "treer-release-test-"));
  const distDir = join(directory, "dist");
  const platform = "linux-x86_64";
  mkdirSync(join(distDir, platform), { recursive: true });
  for (const binary of ["treer", "treer-agent-server", "treer-agent-host"]) {
    const path = join(distDir, platform, binary);
    writeFileSync(path, `fixture ${binary}\n`);
    chmodSync(path, 0o755);
  }
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
    assert.equal(prepared.manifest.artifacts.length, 3);
    assert.deepEqual(
      prepared.manifest.artifacts.map((artifact) => artifact.binary),
      ["treer", "treer-agent-host", "treer-agent-server"],
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
  const data = fixture();
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
