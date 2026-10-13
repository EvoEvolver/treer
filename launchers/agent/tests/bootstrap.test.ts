import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, symlinkSync, readdirSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { hermeticEnv } from "./hermetic.mjs";

const launcher = resolve(import.meta.dirname, ".."), script = readFileSync(join(launcher, "bootstrap.sh"), "utf8");
const realGit = execFileSync("/bin/sh", ["-c", "command -v git"], { encoding: "utf8" }).trim();
function fixture(t: { after: (fn: () => void) => void }) {
  const root = mkdtempSync(join(tmpdir(), "launcher-bootstrap-")), source = join(root, "source"), bin = join(root, "bin"), scratch = join(root, "temporary");
  for (const path of [source, bin, scratch, join(source, "launchers/agent")]) mkdirSync(path, { recursive: true });
  t.after(() => rmSync(root, { recursive: true, force: true }));
  writeFileSync(join(bin, "treer"), "#!/bin/sh\nexit 99\n", { mode: 0o755 }); // No Treer mutations permitted by the fixture.
  writeFileSync(join(bin, "npm"), "#!/bin/sh\nexit 99\n", { mode: 0o755 });
  symlinkSync(process.execPath, join(bin, "node"));
  symlinkSync(realGit, join(bin, "git"));
  symlinkSync("/usr/bin/tar", join(bin, "tar"));
  writeFileSync(join(source, "launchers/agent/install.sh"), `#!/bin/sh\nexec node "$FIXTURE_INSTALL_JS" "$@"\n`);
  const installJs = join(root, "install.cjs");
  writeFileSync(installJs, `const fs=require('node:fs'),cp=require('node:child_process');
const args=process.argv.slice(2),source=args[args.indexOf('--source')+1];
fs.writeFileSync(process.env.FIXTURE_RESULT,JSON.stringify({args,stdin:fs.readFileSync(0,'utf8'),head:cp.execFileSync('git',['-C',source,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),unexpected:fs.existsSync(source+'/unrelated/private.txt')}));`);
  mkdirSync(join(source, "unrelated")); writeFileSync(join(source, "unrelated/private.txt"), "not needed");
  const env = hermeticEnv({ HOME: root, PATH: `${bin}:/usr/bin:/bin`, TMPDIR: scratch, FIXTURE_INSTALL_JS: installJs, FIXTURE_RESULT: join(root, "result"), GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null", GIT_TERMINAL_PROMPT: "0" });
  const git = (...args: string[]) => execFileSync(realGit, ["-C", source, ...args], { env, encoding: "utf8", stdio: "pipe" }).trim();
  git("init", "-q"); git("config", "user.name", "Fixture"); git("config", "user.email", "test@example.invalid"); git("add", "."); git("commit", "-qm", "fixture");
  const sha = git("rev-parse", "HEAD"), repository = `file://${source}`;
  const run = (args: string[] = [], input = script, overrides: NodeJS.ProcessEnv = {}) => spawnSync("/bin/sh", ["-s", "--", "--ref", sha, "--repository", repository, ...args], { input, env: { ...env, ...overrides }, encoding: "utf8" });
  return { root, bin, scratch, sha, source, env, run };
}

test("piped bootstrap sparse-fetches verified SHA, forwards args, closes stdin and cleans temporary files", (t) => {
  const f = fixture(t), result = f.run(["--agent", "codex", "--cwd", "folder with spaces"]);
  assert.equal(result.status, 0, result.stderr);
  const installed = JSON.parse(readFileSync(join(f.root, "result"), "utf8"));
  assert.equal(installed.head, f.sha); assert.equal(installed.stdin, ""); assert.equal(installed.unexpected, false);
  assert.deepEqual(installed.args.slice(2), ["--ref", f.sha, "--repository", `file://${f.source}`, "--agent", "codex", "--cwd", "folder with spaces"]);
  assert.deepEqual(readdirSync(f.scratch), []);
});

test("truncated downloads cannot run a partial installer", (t) => {
  const f = fixture(t), lastCall = script.lastIndexOf("treer_agent_bootstrap \"$@\"");
  for (const cut of [25, Math.floor(script.length / 2), lastCall, lastCall + 10]) {
    f.run(["--agent", "codex"], script.slice(0, cut));
    assert.ok(!readdirSync(f.root).includes("result")); assert.deepEqual(readdirSync(f.scratch), []);
  }
});

test("branch/tag/short/missing refs, unavailable SHA and wrong HEAD never run installer", (t) => {
  const f = fixture(t);
  const missing = spawnSync("/bin/sh", ["-s"], { input: script, env: f.env, encoding: "utf8" });
  assert.notEqual(missing.status, 0); assert.match(missing.stderr, /full 40-character/);
  for (const ref of ["main", "v1", "a".repeat(39), "", "g".repeat(40)]) {
    const result = f.run(["--ref", ref]); assert.notEqual(result.status, 0); assert.match(result.stderr, /full 40-character/);
  }
  assert.notEqual(f.run(["--ref", "0".repeat(40)]).status, 0);
  rmSync(join(f.bin, "git"));
  writeFileSync(join(f.bin, "git"), `#!/bin/sh\ncase "$*" in *'rev-parse HEAD') printf '%s\\n' '${"f".repeat(40)}';; *) exec '${realGit}' "$@";; esac\n`, { mode: 0o755 });
  const mismatch = f.run(); assert.notEqual(mismatch.status, 0); assert.match(mismatch.stderr, /HEAD does not match/);
  assert.ok(!readdirSync(f.root).includes("result")); assert.deepEqual(readdirSync(f.scratch), []);
  assert.match(f.run(["--source", "/unverified"]).stderr, /--source is not allowed/);
});

test("every missing prerequisite has guidance and old Node is rejected before clone", (t) => {
  const f = fixture(t);
  for (const missing of ["treer", "node", "npm", "git", "tar"]) {
    const bin = join(f.root, `missing-${missing}`); mkdirSync(bin);
    for (const tool of ["treer", "node", "npm", "git", "tar"].filter((tool) => tool !== missing)) symlinkSync(join(f.bin, tool), join(bin, tool));
    const result = f.run([], script, { PATH: bin });
    assert.notEqual(result.status, 0); assert.match(result.stderr, new RegExp(`missing ${missing}.*Install`));
  }
  rmSync(join(f.bin, "node")); writeFileSync(join(f.bin, "node"), "#!/bin/sh\nexit 1\n", { mode: 0o755 });
  const old = f.run(); assert.notEqual(old.status, 0); assert.match(old.stderr, /Node.js 22\+ is required/);
  assert.deepEqual(readdirSync(f.scratch), []);
});
