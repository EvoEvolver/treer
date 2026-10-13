import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const script = new URL("./retag-ghcr.sh", import.meta.url).pathname;
const workflows = new URL("../.github/workflows/", import.meta.url);

function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), "treer-release-security-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  writeFileSync(join(directory, "docker"), '#!/bin/sh\nprintf "%s\\n" "$@" > "$DOCKER_ARGS"\n', { mode: 0o755 });
  return {
    directory,
    env: { PATH: `${directory}:/usr/bin:/bin`, HOME: directory,
      MARKER: join(directory, "injection-marker"), DOCKER_ARGS: join(directory, "docker-args"),
      IMAGE: "ghcr.io/example/treer-proxy", TREER_RELEASE_VERSION: "v1.2.3",
      TREER_RELEASE_CHANNEL: "canary" },
  };
}

test("historical inline expression executes input; environment-based retag rejects it", (t) => {
  const { env } = fixture(t);
  const payload = 'v1.2.3$(printf injected > "$MARKER")';
  // Historical workflow substituted this string into shell source before bash ran.
  const old = spawnSync("bash", ["-c", `test "${payload}" != "latest"`], { env, encoding: "utf8" });
  assert.equal(old.status, 0, old.stderr);
  assert.equal(readFileSync(env.MARKER, "utf8"), "injected");
  rmSync(env.MARKER);
  const fixed = spawnSync("bash", [script, "apply"], {
    env: { ...env, TREER_RELEASE_VERSION: payload }, encoding: "utf8",
  });
  assert.notEqual(fixed.status, 0);
  assert.equal(existsSync(env.MARKER), false);
  assert.equal(existsSync(env.DOCKER_ARGS), false);
});

test("retag validates both fields before touching registry and preserves exact safe argv", (t) => {
  const { env } = fixture(t);
  for (const overrides of [
    { TREER_RELEASE_VERSION: "latest" }, { TREER_RELEASE_VERSION: "v1.2.3\nmalicious" },
    { TREER_RELEASE_VERSION: 'v1.2.3"; false; #' }, { TREER_RELEASE_VERSION: "--help" },
    { TREER_RELEASE_CHANNEL: "production" }, { TREER_RELEASE_CHANNEL: "stable; false" },
    { IMAGE: "ghcr.io/example/treer-proxy:other" },
  ]) {
    const result = spawnSync("bash", [script, "apply"], { env: { ...env, ...overrides } });
    assert.notEqual(result.status, 0);
    assert.equal(existsSync(env.DOCKER_ARGS), false);
  }
  for (const channel of ["canary", "stable"]) {
    const result = spawnSync("bash", [script, "apply"], {
      env: { ...env, TREER_RELEASE_CHANNEL: channel }, encoding: "utf8",
    });
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(readFileSync(env.DOCKER_ARGS, "utf8").trim().split("\n"),
      ["buildx", "imagetools", "create", "-t", `${env.IMAGE}:${channel}`, `${env.IMAGE}:v1.2.3`]);
    rmSync(env.DOCKER_ARGS);
  }
});

test("workflows pin all external actions and never persist checkout credentials", () => {
  for (const file of readdirSync(workflows).filter((name) => /\.ya?ml$/.test(name))) {
    const text = readFileSync(new URL(file, workflows), "utf8");
    for (const match of text.matchAll(/uses:\s*([^\s#]+)/g)) {
      if (match[1].startsWith("./")) continue;
      assert.match(match[1], /^[\w.-]+\/[\w.-]+@[0-9a-f]{40}$/, `${file}: ${match[1]}`);
    }
    const lines = text.split("\n");
    for (let i = 0; i < lines.length; i++) {
      if (lines[i].includes("uses: actions/checkout@")) {
        assert.match(lines.slice(i + 1, i + 4).join("\n"), /persist-credentials: false/, file);
      }
    }
    assert.doesNotMatch(text, /pull_request_target/, file);
  }
  const publish = readFileSync(new URL("publish-ghcr.yml", workflows), "utf8");
  // Every run block must treat dispatch input as env data, not shell source.
  for (const block of publish.matchAll(/run:\s*\|\n((?: {10}.*\n|\n)*)/g)) {
    assert.doesNotMatch(block[1], /\$\{\{\s*inputs\./);
  }
  assert.ok(publish.indexOf("retag-ghcr.sh validate") < publish.lastIndexOf("Log in to GHCR"));
});
