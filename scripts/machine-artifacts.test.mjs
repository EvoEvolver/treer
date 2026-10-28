import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFile, spawnSync } from "node:child_process";
import { createServer } from "node:http";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";

const commit = "a".repeat(40);
const platforms = ["linux-x86_64", "linux-aarch64", "darwin-x86_64", "darwin-aarch64", "windows-x86_64"];
const binaries = ["treer", "treer-launch", "treer-agent-host", "treer-agent-server"];
const verifier = new URL("./verify-machine-artifacts.sh", import.meta.url).pathname;
const servedVerifier = new URL("./verify-served-artifacts.sh", import.meta.url).pathname;
const exec = promisify(execFile);

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "treer-artifact-test-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const platform of platforms) {
    const directory = join(root, platform);
    mkdirSync(directory);
    writeFileSync(join(directory, "build-metadata.json"), JSON.stringify({
      schema_version: 1, git_commit: commit, platform, version: "0.1.13",
    }));
    const hashes = binaries.map((binary) => {
      const bytes = Buffer.from(`${platform.startsWith("windows-") ? "MZ" : ""}${platform}/${binary}`);
      writeFileSync(join(directory, binary), bytes, { mode: 0o755 });
      return `${createHash("sha256").update(bytes).digest("hex")}  ${binary}\n`;
    });
    writeFileSync(join(directory, "SHA256SUMS"), hashes.join(""));
  }
  return root;
}

for (const mode of ["matching", "redirect", "stale"]) {
  test(`public downloads: ${mode}`, async (t) => {
    const root = fixture(t);
    let requests = 0;
    const server = createServer((request, response) => {
      requests++;
      const [, , platform, binary] = request.url.split("/");
      if (mode === "redirect" && platform === "darwin-aarch64") {
        response.writeHead(307, { Location: "https://example.invalid/old-release" });
        response.end();
      } else if (mode === "stale" && platform === "darwin-aarch64") {
        response.end("old macOS binary");
      } else {
        response.end(readFileSync(join(root, platform, binary)));
      }
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    t.after(() => new Promise((resolve) => server.close(resolve)));
    const url = `http://127.0.0.1:${server.address().port}`;
    const check = exec("sh", [servedVerifier, url, root]);
    if (mode === "matching") {
      await check;
      assert.equal(requests, 20);
    } else {
      await assert.rejects(check);
    }
  });
}

function verify(root) {
  return spawnSync("sh", [verifier, commit, root], { encoding: "utf8" });
}

test("accepts complete five-platform artifacts", (t) => {
  const result = verify(fixture(t));
  assert.equal(result.status, 0, result.stderr);
});

for (const [name, mutate] of [
  ["missing Windows binary", (root) => rmSync(join(root, "windows-x86_64", "treer-agent-host"))],
  ["invalid Windows executable", (root) => writeFileSync(join(root, "windows-x86_64", "treer"), "ELF")],
  ["missing macOS binary", (root) => rmSync(join(root, "darwin-aarch64", "treer-agent-server"))],
  ["changed binary bytes", (root) => writeFileSync(join(root, "darwin-x86_64", "treer"), "old")],
  ["different source commit", (root) => {
    const path = join(root, "darwin-aarch64", "build-metadata.json");
    const metadata = JSON.parse(readFileSync(path));
    metadata.git_commit = "b".repeat(40);
    writeFileSync(path, JSON.stringify(metadata));
  }],
  ["different platform version", (root) => {
    const path = join(root, "linux-aarch64", "build-metadata.json");
    const metadata = JSON.parse(readFileSync(path));
    metadata.version = "0.0.1";
    writeFileSync(path, JSON.stringify(metadata));
  }],
  ["missing executable bit", (root) => chmodSync(join(root, "darwin-aarch64", "treer"), 0o644)],
  ["partial checksum manifest", (root) => {
    const path = join(root, "darwin-aarch64", "SHA256SUMS");
    writeFileSync(path, readFileSync(path, "utf8").split("\n")[0] + "\n");
  }],
  ["duplicate checksum entry", (root) => {
    const path = join(root, "darwin-aarch64", "SHA256SUMS");
    const lines = readFileSync(path, "utf8").split("\n");
    writeFileSync(path, [lines[0], lines[0], lines[2]].join("\n") + "\n");
  }],
]) {
  test(`rejects ${name}`, (t) => {
    const root = fixture(t);
    mutate(root);
    assert.notEqual(verify(root).status, 0);
  });
}
