import assert from "node:assert/strict";
import { spawnSync, execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { nativeRuntime, runtimeStatus } from "../scripts/native-runtime.mjs";
import { nativeBindingFixture } from "./native-binding-fixture.mjs";
import { hermeticEnv } from "./hermetic.mjs";

const launcher = resolve(import.meta.dirname, "..");
function fixture(t: any, executable?: string) {
  const root = realpathSync.native(mkdtempSync(join(tmpdir(), "native-default-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "scripts"));
  for (const name of ["treer-agent.mjs", "native-runtime.mjs", "process.mjs"]) copyFileSync(join(launcher, "scripts", name), join(root, "scripts", name));
  const binding = nativeBindingFixture(root, join(launcher, "public"), executable);
  writeFileSync(join(root, "native-runtime.json"), JSON.stringify(binding.value), { mode: 0o600 });
  const runEnv = (extra: Record<string, string>, ...args: string[]) => spawnSync(process.execPath, [join(root, "scripts/treer-agent.mjs"), ...args], { encoding: "utf8", env: hermeticEnv(extra), timeout: 10000 });
  const run = (...args: string[]) => runEnv({}, ...args);
  return { root, binding: binding.value, run, runEnv, save: () => writeFileSync(join(root, "native-runtime.json"), JSON.stringify(binding.value)) };
}
test("default dispatcher executes the pinned native image with session/cwd/harness and no Node runtime", { skip: process.platform === "win32" }, t => {
  const build = realpathSync.native(mkdtempSync(join(tmpdir(), "native-probe-")));
  t.after(() => rmSync(build, { recursive: true, force: true }));
  const source = join(build, "probe.c"), executable = join(build, "probe");
  writeFileSync(source, '#include <stdio.h>\n#include <stdlib.h>\nint main(int argc,char **argv){printf("%s|%s|%s|%s|%d|%s\\n",getenv("AGENT_UI_HARNESS"),getenv("AGENT_UI_SESSION_ID"),getenv("AGENT_UI_CWD"),getenv("RUST_SPIKE_REGISTER"),argc,getenv("AGENT_UI_PORT_FILE")?"inherited":"absent");return 0;}\n');
  execFileSync("cc", [source, "-o", executable]);
  const f = fixture(t, executable);
  for (const harness of ["codex", "claude"]) {
    const result = f.run("--harness", harness, "--session-id", "same-session", "--cwd", build);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, `${harness}|same-session|${build}|1|1|absent\n`);
  }
  const inherited = join(build, "parent-port"); writeFileSync(inherited, "unchanged");
  const compatibility = f.runEnv({ CODEX_UI_CWD: build, AGENT_UI_PORT_FILE: inherited }, "--session-id", "same-session");
  assert.equal(compatibility.status, 0, compatibility.stderr);
  assert.equal(compatibility.stdout, `codex|same-session|${build}|1|1|absent\n`);
  assert.equal(readFileSync(inherited, "utf8"), "unchanged");
});
test("missing binding, changed image, wrong platform and missing provider fail with no fallback", t => {
  const f = fixture(t);
  f.binding.platform = "not-this-platform"; f.save();
  assert.throws(() => nativeRuntime(f.root), /no Node fallback/);
  f.binding.platform = process.platform; f.binding.executable.sha256 = "0".repeat(64); f.save();
  assert.notEqual(f.run().status, 0);
  f.binding.executable.sha256 = f.binding.tools.treer.sha256; delete f.binding.tools.claude; f.save();
  assert.equal(runtimeStatus(f.root).claude, "unavailable-provider");
  assert.notEqual(f.run("--harness", "claude").status, 0);
  rmSync(join(f.root, "native-runtime.json"));
  assert.equal(runtimeStatus(f.root).default, "unconfigured");
  assert.notEqual(f.run().status, 0);
});
test("ACP requires explicit Node selection and preserves provider argv", t => {
  const f = fixture(t);
  writeFileSync(join(f.root, "scripts/treer-agent-node.mjs"), 'console.log(JSON.stringify(process.argv.slice(2)))');
  assert.match(f.run("--runtime", "rust", "--harness", "acp").stderr, /explicit --runtime node/);
  const result = f.run("--runtime", "node", "--harness", "acp", "--acp-command", "provider", "--acp-arg", "value with spaces");
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), ["--harness", "acp", "--acp-command", "provider", "--acp-arg", "value with spaces"]);
  const existingProfile = f.run("--harness", "acp", "--acp-command", "provider");
  assert.equal(existingProfile.status, 0, existingProfile.stderr);
  assert.deepEqual(JSON.parse(existingProfile.stdout), ["--harness", "acp", "--acp-command", "provider"]);
  const profiles = JSON.parse(readFileSync(join(launcher, "profiles.json"), "utf8")).profiles;
  for (const p of profiles.filter((p: any) => !["codex", "claude"].includes(p.id))) assert.match(p.description, /explicit Node/);
});

test("split UI native bindings refuse a missing worker even when core assets match", t => {
  const f = fixture(t);
  assert.ok(f.binding.assets.some((asset: { path: string }) => /pdf-worker-/.test(asset.path)));
  assert.doesNotThrow(() => nativeRuntime(f.root));
  f.binding.assets = f.binding.assets.filter((asset: { path: string }) => !/pdf-worker-/.test(asset.path)); f.save();
  assert.throws(() => nativeRuntime(f.root), /no Node fallback/);
});
