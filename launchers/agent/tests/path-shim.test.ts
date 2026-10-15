import assert from "node:assert/strict";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { shimPath, shimText } from "../scripts/path-shim.mjs";
import { windowsShimSource } from "../scripts/windows-shim.mjs";
import { hermeticEnv } from "./hermetic.mjs";

function fixture(t: { after: (fn: () => void) => void }) {
  const root = realpathSync.native(mkdtempSync(join(tmpdir(), "path-shim-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const entry = join(root, "entry with spaces ' quote.mjs");
  writeFileSync(entry, "console.log(JSON.stringify(process.argv.slice(2))); process.exitCode=7;");
  const env = hermeticEnv({ HOME: root, LOCALAPPDATA: root, PATH: dirname(process.execPath) });
  const call = (text = "installShim(process.execPath, process.argv[1])") => execFileSync(process.execPath, ["--input-type=module", "-e", `import {installShim} from ${JSON.stringify(new URL("../scripts/path-shim.mjs", import.meta.url).href)}; ${text}`, entry], { env, stdio: "pipe" });
  return { root, entry, env, call, path: shimPath(env) };
}
test("installed entry pins Node, preserves argv/cwd/exit status without PATH Node, and is idempotent", t => {
  const f = fixture(t); f.call();
  const before = readFileSync(f.path);
  f.call(); assert.deepEqual(readFileSync(f.path), before);
  const args = ["", "hello world $ &", 'quotes " backslash\\', "a\\", "%PATH%", "!bang!", "你好"];
  const result = spawnSync(f.path, args, { cwd: f.root, env: { ...f.env, PATH: "" }, encoding: "utf8", shell: false });
  assert.equal(result.status, 7, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), args);
});
test("shim rollback restores previous pinned entry and never edits neighboring files", t => {
  const f = fixture(t); f.call(); const before = readFileSync(f.path);
  const neighbor = join(dirname(f.path), "owner-program"); writeFileSync(neighbor, "owner");
  f.call("const change=installShim(process.execPath,process.argv[1]+'-new'); change.rollback();");
  assert.deepEqual(readFileSync(f.path), before);
  assert.equal(readFileSync(neighbor, "utf8"), "owner");
});
test("unknown existing executable and symlink entry are refused", t => {
  const f = fixture(t); mkdirSync(dirname(f.path), { recursive: true }); writeFileSync(f.path, "owner executable");
  assert.throws(() => f.call(), /not a managed launcher shim/);
  assert.equal(readFileSync(f.path, "utf8"), "owner executable");
  rmSync(f.path); symlinkSync(f.entry, f.path);
  assert.throws(() => f.call(), /owned regular shim/);
});
test("linked bin directory is refused rather than writing into its target", t => {
  const f = fixture(t), parent = dirname(dirname(f.path)), outside = join(f.root, "outside");
  mkdirSync(parent, { recursive: true }); mkdirSync(outside); symlinkSync(outside, dirname(f.path), process.platform === "win32" ? "junction" : "dir");
  assert.throws(() => f.call(), /not links/);
  assert.equal(existsSync(join(outside, "treer-agent")), false);
});
test("macOS and Linux shell shims quote fixed paths and use one exact argument vector", () => {
  for (const platform of ["darwin", "linux"]) {
    assert.equal(shimText("/node path/node", "/root's dir/run.mjs", platform), "#!/bin/sh\n# treer-agent-shim/v1\nexec '/node path/node' '/root'\\''s dir/run.mjs' \"$@\"\n");
  }
  assert.throws(() => shimText("/node", "/entry\nexec evil", "linux"), /control/);
});
test("Windows native source uses no shell, inherits lifecycle and correctly quotes argument vectors", () => {
  const source = windowsShimSource('C:\\node path\\node.exe', 'C:\\root\\run.mjs');
  assert.match(source, /AssignProcessToJobObject/);
  assert.match(source, /KILL_ON_JOB_CLOSE/);
  assert.match(source, /return unchecked\(\(int\)code\)/);
  assert.ok(!source.includes("cmd.exe"));
  assert.ok(!source.includes("EnvironmentVariables"));
});
test("missing Windows framework compiler reports its prerequisite without a shell fallback", t => {
  const f = fixture(t);
  const url = new URL("../scripts/windows-shim.mjs", import.meta.url).href;
  // Node's Windows crypto initialization itself needs the real SystemRoot.
  // Change it only after Node starts, to isolate compiler discovery.
  assert.throws(() => execFileSync(process.execPath, ["--input-type=module", "-e", `import {buildWindowsShim} from ${JSON.stringify(url)}; process.env.SystemRoot=process.argv[2]; process.env.SYSTEMROOT=process.argv[2]; buildWindowsShim(process.execPath,process.argv[1],process.argv[2]);`, f.entry, f.root], { env: f.env, stdio: "pipe" }), /Windows .NET Framework 4 compiler is required/);
});
test("Windows stopping only the PE kills its Node and grandchild, and a fresh launch still works", { skip: process.platform !== "win32" }, async t => {
  const f = fixture(t), pids = join(f.root, "pids.json");
  writeFileSync(f.entry, `import fs from 'node:fs';import {spawn} from 'node:child_process';const child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'inherit'});fs.writeFileSync(${JSON.stringify(pids)},JSON.stringify({parent:process.pid,child:child.pid}));setInterval(()=>{},1000);`);
  f.call();
  const alive = (pid: number) => { try { process.kill(pid, 0); return true; } catch { return false; } };
  for (let generation = 0; generation < 2; generation++) {
    rmSync(pids, { force: true });
    const child = spawn(f.path, [], { env: f.env, stdio: "ignore" });
    t.after(() => { if (child.exitCode === null && child.signalCode === null) child.kill(); });
    const deadline = Date.now() + 10000;
    while (!existsSync(pids) && Date.now() < deadline && child.exitCode === null) await new Promise(done => setTimeout(done, 20));
    assert.ok(existsSync(pids), "native child never became ready");
    const running = JSON.parse(readFileSync(pids, "utf8"));
    assert.ok(alive(running.parent)); assert.ok(alive(running.child));
    child.kill(); // Only the recorded wrapper PID, not taskkill /T.
    while ((alive(running.parent) || alive(running.child)) && Date.now() < deadline) await new Promise(done => setTimeout(done, 20));
    assert.equal(alive(running.parent), false); assert.equal(alive(running.child), false);
  }
});
