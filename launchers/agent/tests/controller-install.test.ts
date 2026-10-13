import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { after, test } from "node:test";
import { cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { compatibility, digest } from "../scripts/bundle-contract.mjs";
import { hermeticEnv } from "./hermetic.mjs";
const launcher = resolve(import.meta.dirname, "..");
const temp = () => realpathSync(mkdtempSync(join(tmpdir(), "controller-launcher-")));
const prepared = temp();
execFileSync(process.execPath, [join(launcher, "scripts/prepare-bundle.mjs"), "--output", prepared, "--source-commit", "a".repeat(40)], { env: hermeticEnv(), stdio: "pipe" });
after(() => rmSync(prepared, { recursive: true, force: true }));
const mock = `#!/usr/bin/env node
const fs=require('node:fs'),path=require('node:path'),root=process.env.FIXTURE_ROOT,args=process.argv.slice(2);
fs.appendFileSync(path.join(root,'calls'),JSON.stringify(args)+'\\n');
if(args.join(' ')==='interface contract'){console.log(fs.readFileSync(path.join(root,'controller.json'),'utf8'));process.exit();}
if(args[0]==='interface'){console.log('{}');process.exit();}
if(args.slice(0,3).join(' ')!=='agent admin profile')throw Error('Unexpected operation');
const [action,name]=args.slice(3),file=path.join(root,name+'.json');
if(action==='show'){if(!fs.existsSync(file)){console.error('profile_not_found');process.exit(1)}const saved=JSON.parse(fs.readFileSync(file));if(process.env.BAD_SHOW===name)saved.args=[];console.log(JSON.stringify(saved));}
else if(action==='create'||action==='update'){const i=args.indexOf('--cwd'),command=action==='create'?args[i+2]:args[args.indexOf('--command')+1],tail=action==='create'?args.slice(args.indexOf('--')+1):args.flatMap((v,i)=>v==='--arg'?[args[i+1]]:[]);fs.writeFileSync(file,JSON.stringify({cwd:args[i+1],command,args:tail}));}
else if(action==='delete')fs.unlinkSync(file);else throw Error('Unexpected operation');
`;
function fixture(t: { after: (fn: () => void) => void }) {
  const root = temp(), machine = join(root, "machine"), bundle = join(root, "bundle"), treer = join(root, "treer");
  mkdirSync(machine); cpSync(prepared, bundle, { recursive: true }); writeFileSync(treer, mock, { mode: 0o700 });
  const contract = JSON.parse(readFileSync(join(bundle, "interface-contract.json"), "utf8"));
  const controller = { git_commit: "a".repeat(40), protocol: contract.protocol, capabilities: contract.capabilities, features: contract.required_features };
  writeFileSync(join(root, "controller.json"), JSON.stringify(controller));
  const request = { schema: "treer.launcher-install/v1", operation: "install", bundle_dir: bundle,
    version: `sha256-${digest(readFileSync(join(bundle, "bundle-manifest.json")))}`, source_commit: "a".repeat(40), root: machine, cwd: ".", agents: [], node: process.execPath, treer, controller, allow_network: false };
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const call = (overrides = {}, env = {}) => JSON.parse(execFileSync(process.execPath, [join(launcher, "scripts/install.mjs"), "--controller-request"], { input: JSON.stringify({ ...request, ...overrides }), encoding: "utf8", env: hermeticEnv({ FIXTURE_ROOT: root, PATH: dirname(process.execPath), ...env }), stdio: "pipe" }));
  return { root, machine, bundle, request, call, base: join(machine, ".treer/launchers/agent") };
}
function revise(f: ReturnType<typeof fixture>) {
  const path = join(f.bundle, "public/index.html"); writeFileSync(path, readFileSync(path, "utf8") + "\n<!--next-->\n");
  const manifestPath = join(f.bundle, "bundle-manifest.json"), manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  const file = manifest.files.find((entry: any) => entry.path === "public/index.html"), bytes = readFileSync(path); file.size = bytes.length; file.sha256 = digest(bytes);
  writeFileSync(manifestPath, JSON.stringify(manifest)); f.request.version = `sha256-${digest(readFileSync(manifestPath))}`;
}
test("portable install is offline, saves all six profiles, idempotently retains assets and rolls back", (t) => {
  const f = fixture(t), result = f.call();
  assert.equal(result.portable, true); assert.equal(result.profiles.length, 6);
  const installed = join(f.base, f.request.version); assert.equal(existsSync(join(installed, "node_modules")), false);
  const profile = JSON.parse(readFileSync(join(f.root, "Codex + UI.json"), "utf8"));
  assert.equal(profile.command, process.execPath); assert.deepEqual(profile.args, [join(f.base, "run.mjs"), "--harness", "codex"]);
  const bytes = readFileSync(join(installed, "runtime.mjs")); f.call(); assert.deepEqual(readFileSync(join(installed, "runtime.mjs")), bytes);
  const previous = f.request.version; revise(f); f.call({ operation: "update" });
  assert.equal(f.call({ operation: "status" }).previous, previous);
  f.call({ operation: "rollback" }); assert.equal(f.call({ operation: "status" }).current, previous);
  assert.deepEqual(readFileSync(join(installed, "runtime.mjs")), bytes);
  if (process.platform !== "win32") { assert.equal(lstatSync(f.base).mode & 0o777, 0o700); assert.equal(lstatSync(join(f.base, "current.json")).mode & 0o777, 0o600); }
});
test("failure verifying a later profile restores earlier profiles and the current pointer", (t) => {
  const f = fixture(t); f.call(); const pointer = readFileSync(join(f.base, "current.json"), "utf8"), profile = readFileSync(join(f.root, "Codex + UI.json"), "utf8");
  revise(f); assert.throws(() => f.call({ operation: "update" }, { BAD_SHOW: "Claude + UI" }), /Profile verification failed/);
  assert.equal(readFileSync(join(f.base, "current.json"), "utf8"), pointer); assert.equal(readFileSync(join(f.root, "Codex + UI.json"), "utf8"), profile);
});
test("capability disagreement, unknown old Controller and rollback incompatibility fail before mutation", (t) => {
  const f = fixture(t);
  assert.throws(() => f.call({ controller: { ...f.request.controller, capabilities: [] } }), /lacks launcher/);
  assert.equal(existsSync(join(f.root, "calls")), false);
  f.call(); assert.throws(() => f.call({ operation: "rollback", rollback_version: f.request.version, controller: { ...f.request.controller, features: [] } }), /lacks launcher/);
  assert.equal(compatibility({}, null).status, "unknown");
  assert.equal(f.call({ operation: "status", controller: null }).versions[0].compatibility.status, "unknown");
});
test("bundle integrity, path traversal, symlink escape and concurrent locks fail closed", (t) => {
  const f = fixture(t); writeFileSync(join(f.bundle, "runtime.mjs"), "unverified code");
  assert.throws(() => f.call(), /integrity mismatch/); assert.equal(existsSync(join(f.root, "calls")), false);
  const g = fixture(t); symlinkSync(g.root, join(g.machine, ".treer"), "dir"); assert.throws(() => g.call(), /link/);
  const h = fixture(t); h.call(); mkdirSync(join(h.base, ".install-lock")); assert.throws(() => h.call(), /owns the lock/);
  const k = fixture(t); const manifest = JSON.parse(readFileSync(join(k.bundle, "bundle-manifest.json"), "utf8")); manifest.files[0].path = "../escape";
  writeFileSync(join(k.bundle, "bundle-manifest.json"), JSON.stringify(manifest)); k.request.version = `sha256-${digest(readFileSync(join(k.bundle, "bundle-manifest.json")))}`;
  assert.throws(() => k.call(), /manifest entry/);
  const m = fixture(t); writeFileSync(join(m.bundle, "extra.js"), "not listed"); assert.throws(() => m.call(), /integrity mismatch/);
});
test("portable runtime and Node entry register the same interface contract without shell or node_modules", async (t) => {
  const f = fixture(t); f.call();
  const child = spawn(process.execPath, [join(f.base, "run.mjs"), "--harness", "codex"], { cwd: f.machine, env: hermeticEnv({ FIXTURE_ROOT: f.root, PATH: dirname(process.execPath), CODEX_BIN: join(launcher, "tests/fake-codex.mjs"), AGENT_UI_MESSAGE_WATCH: "0" }), stdio: "pipe" });
  let error = ""; child.stderr.on("data", (chunk) => error += chunk);
  t.after(() => child.kill());
  const deadline = Date.now() + 10000;
  let registration;
  while (Date.now() < deadline && child.exitCode === null) {
    const calls = readFileSync(join(f.root, "calls"), "utf8").trim().split("\n").map((line) => JSON.parse(line));
    registration = calls.find((args) => args[0] === "interface" && args[1] === "register");
    if (registration) break; await new Promise((done) => setTimeout(done, 25));
  }
  assert.ok(registration, `portable registration missing: ${error}`);
  assert.deepEqual(registration.flatMap((arg: string, i: number) => arg === "--capability" ? [registration[i + 1]] : []).sort(), f.request.controller.capabilities.slice().sort());
  const exit = once(child, "exit"); child.kill(); await exit;
});
