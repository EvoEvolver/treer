import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { after, test } from "node:test";
import { cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { compatibility, digest } from "../scripts/bundle-contract.mjs";
import { hermeticEnv } from "./hermetic.mjs";
import { controllerContractFixture } from "./controller-contract.mjs";
import { nativeBindingFixture } from "./native-binding-fixture.mjs";
const launcher = resolve(import.meta.dirname, "..");
const temp = () => realpathSync(mkdtempSync(join(tmpdir(), "controller-launcher-")));
const prepared = temp();
const bindingDirectory = temp(), native = nativeBindingFixture(bindingDirectory, join(launcher, "public"));
after(() => rmSync(bindingDirectory, { recursive: true, force: true }));
execFileSync(process.execPath, [join(launcher, "scripts/prepare-bundle.mjs"), "--output", prepared, "--source-commit", "a".repeat(40), "--native-manifest", native.path, "--native-sha256", native.sha256], { env: hermeticEnv(), stdio: "pipe" });
after(() => rmSync(prepared, { recursive: true, force: true }));
const mock = `#!/usr/bin/env node
const fs=require('node:fs'),path=require('node:path'),root=process.env.FIXTURE_ROOT,args=process.argv.slice(2);
fs.appendFileSync(path.join(root,'calls'),JSON.stringify(args)+'\\n');
if(args.join(' ')==='interface contract'){console.log(fs.readFileSync(path.join(root,'controller.json'),'utf8'));process.exit();}
if(args[0]==='whoami'){console.error(JSON.stringify({error:{code:'cli_failed',message:'TREER_AGENT_ID is not set'}}));process.exit(1);}
if(args.join(' ')==='machine list'){console.log(JSON.stringify([JSON.parse(fs.readFileSync(path.join(root,'controller.json'),'utf8')).machine]));process.exit();}
if(args[0]==='interface'){console.log('{}');process.exit();}
if(args.slice(0,3).join(' ')!=='agent admin profile')throw Error('Unexpected operation');
const [action,name]=args.slice(3),file=path.join(root,name+'.json');
if(action==='show'){if(!fs.existsSync(file)){console.error('profile_not_found');process.exit(1)}const saved=JSON.parse(fs.readFileSync(file));if(process.env.BAD_SHOW===name&&fs.existsSync(path.join(root,'bad-'+name)))saved.args=[];console.log(JSON.stringify(saved));}
else if(action==='create'||action==='update'){const i=args.indexOf('--cwd'),command=action==='create'?args[i+2]:args[args.indexOf('--command')+1],tail=action==='create'?args.slice(args.indexOf('--')+1):args.flatMap((v,i)=>v==='--arg'?[args[i+1]]:[]);fs.writeFileSync(file,JSON.stringify({cwd:args[i+1],command,args:tail}));if(process.env.BAD_SHOW===name)fs.writeFileSync(path.join(root,'bad-'+name),'1');}
else if(action==='delete')fs.unlinkSync(file);else throw Error('Unexpected operation');
`;
function fixture(t: { after: (fn: () => void) => void }) {
  const root = temp(), machine = join(root, "machine"), bundle = join(root, "bundle"), treer = join(root, "treer");
  mkdirSync(machine); cpSync(prepared, bundle, { recursive: true }); writeFileSync(treer, mock, { mode: 0o700 });
  const contract = JSON.parse(readFileSync(join(bundle, "interface-contract.json"), "utf8"));
  const controller = controllerContractFixture(contract, { root: machine, name: "win_native", server_id: "srv_fixture", workspace_id: "ws_fixture" });
  writeFileSync(join(root, "controller.json"), JSON.stringify(controller));
  const request = { schema: "treer.launcher-install/v1", operation: "install", bundle_dir: bundle,
    version: `sha256-${digest(readFileSync(join(bundle, "bundle-manifest.json")))}`, source_commit: "a".repeat(40), root: machine, cwd: ".", agents: [], node: process.execPath, treer, controller, allow_network: false };
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const call = (overrides = {}, env = {}) => JSON.parse(execFileSync(process.execPath, [join(launcher, "scripts/install.mjs"), "--controller-request"], { input: JSON.stringify({ ...request, ...overrides }), encoding: "utf8", env: hermeticEnv({ HOME: root, LOCALAPPDATA: root, FIXTURE_ROOT: root, PATH: dirname(process.execPath), ...env }), stdio: "pipe" }));
  return { root, machine, bundle, request, call, base: join(machine, ".treer/launchers/agent") };
}
function revise(f: ReturnType<typeof fixture>) {
  const path = join(f.bundle, "public/index.html"); writeFileSync(path, readFileSync(path, "utf8") + "\n<!--next-->\n");
  const manifestPath = join(f.bundle, "bundle-manifest.json"), manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  const file = manifest.files.find((entry: any) => entry.path === "public/index.html"), bytes = readFileSync(path); file.size = bytes.length; file.sha256 = digest(bytes);
  writeFileSync(manifestPath, JSON.stringify(manifest)); f.request.version = `sha256-${digest(readFileSync(manifestPath))}`;
}
test("installer and native-smoke Controller fixtures advertise optional private-state without requiring it for text-only launchers", () => {
  const contract = JSON.parse(readFileSync(join(launcher, "interface-contract.json"), "utf8"));
  const controller = controllerContractFixture(contract, { root: prepared, server_id: "srv_fixture", workspace_id: "ws_fixture" });
  assert.ok(controller.features.includes("agent.private-state.v1"));
  assert.equal(new Set(controller.features).size, controller.features.length);
  assert.equal(contract.required_features.includes("agent.private-state.v1"), false);
  assert.equal(compatibility(contract, controller).status, "compatible");
  assert.equal(compatibility(contract, { ...controller, features: contract.required_features }).status, "compatible");
});
test("portable install is offline, saves all six profiles, idempotently retains assets and rolls back", (t) => {
  const f = fixture(t), result = f.call();
  assert.equal(result.portable, true); assert.equal(result.profiles.length, 6);
  assert.equal(f.call({ operation: "status" }).current_runtime.default, "rust");
  assert.equal(f.call({ operation: "status" }).current_runtime.acp, "explicit-node");
  const installed = join(f.base, f.request.version); assert.equal(existsSync(join(installed, "node_modules")), false);
  assert.deepEqual(readFileSync(join(installed, "provider-recipes.json")), readFileSync(join(launcher, "provider-recipes.json")));
  const manifest = JSON.parse(readFileSync(join(installed, "bundle-manifest.json"), "utf8"));
  assert.equal(manifest.files.find((file: any) => file.path === "provider-recipes.json").sha256, digest(readFileSync(join(installed, "provider-recipes.json"))));
  const profile = JSON.parse(readFileSync(join(f.root, "Codex + UI.json"), "utf8"));
  assert.equal(profile.command, "treer-agent"); assert.deepEqual(profile.args, ["--harness", "codex"]);
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
test("isolated skip-profiles installation does not create or redirect a per-user default shim", (t) => {
  const f = fixture(t), result = f.call({ skip_profiles: true });
  assert.equal(result.shim, null); assert.deepEqual(result.profiles, []);
  assert.equal(existsSync(join(f.root, ".local/bin/treer-agent")), false);
  assert.equal(existsSync(join(f.root, "Treer/bin/treer-agent.exe")), false);
  assert.equal(existsSync(join(f.base, "profiles-receipt.json")), false);
});
test("a native-default payload without its binding refuses before pointer/profile/default changes", t => {
  const f = fixture(t), path = join(f.bundle, "bundle-manifest.json");
  const manifest = JSON.parse(readFileSync(path, "utf8"));
  manifest.files = manifest.files.filter((file: any) => file.path !== "native-runtime.json");
  rmSync(join(f.bundle, "native-runtime.json")); writeFileSync(path, JSON.stringify(manifest));
  f.request.version = `sha256-${digest(readFileSync(path))}`;
  assert.throws(() => f.call(), /requires a verified machine-specific native-runtime/);
  assert.equal(existsSync(join(f.base, "current.json")), false);
  assert.equal(existsSync(join(f.root, "Codex + UI.json")), false);
  assert.equal(existsSync(join(f.root, ".local/bin/treer-agent")), false);
});
test("recognized profiles from another machine converge to one portable profile through update and rollback", (t) => {
  const f = fixture(t), profiles = JSON.parse(readFileSync(join(f.bundle, "profiles.json"), "utf8")).profiles;
  for (const profile of profiles) writeFileSync(join(f.root, profile.name + ".json"), JSON.stringify({ command: "/Users/mac/.treer/launchers/agent/current/launchers/agent/scripts/treer-agent.sh", cwd: "treer", args: profile.args }));
  const first = f.request.version, names = profiles.map((p: any) => p.name);
  assert.deepEqual(f.call().profiles, names);
  const receipt = JSON.parse(readFileSync(join(f.base, "profiles-receipt.json"), "utf8"));
  assert.equal(receipt.$profile_names.codex, "Codex + UI");
  revise(f);
  assert.deepEqual(f.call({ operation: "update" }).profiles, names);
  assert.deepEqual(f.call({ operation: "rollback", rollback_version: first }).profiles, names);
  for (const profile of profiles) assert.deepEqual(JSON.parse(readFileSync(join(f.root, profile.name + ".json"), "utf8")), { command: "treer-agent", cwd: ".", args: profile.args });
});
test("custom profiles are protected rather than creating more machine-qualified names", (t) => {
  const f = fixture(t), foreign = JSON.stringify({ command: "/owner/script", cwd: ".", args: [] });
  writeFileSync(join(f.root, "Codex + UI.json"), foreign);
  assert.throws(() => f.call({ agents: ["codex"] }), /customized/);
  assert.equal(readFileSync(join(f.root, "Codex + UI.json"), "utf8"), foreign);
  assert.equal(existsSync(join(f.base, "current.json")), false);
});
test("prototype names do not authorize adoption of custom profiles", (t) => {
  for (const name of ["constructor", "__proto__", "toString"]) {
    const f = fixture(t), path = join(f.bundle, "profiles.json"), profiles = JSON.parse(readFileSync(path, "utf8"));
    profiles.profiles.find((p: any) => p.id === "codex").name = name;
    writeFileSync(path, JSON.stringify(profiles));
    const manifestPath = join(f.bundle, "bundle-manifest.json"), manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    const entry = manifest.files.find((file: any) => file.path === "profiles.json"), bytes = readFileSync(path);
    entry.size = bytes.length; entry.sha256 = digest(bytes); writeFileSync(manifestPath, JSON.stringify(manifest));
    f.request.version = `sha256-${digest(readFileSync(manifestPath))}`;
    assert.deepEqual(f.call({ agents: ["codex"] }).profiles, [name]);
    assert.deepEqual(f.call({ agents: ["codex"], operation: "update" }).profiles, [name]);
    const custom = JSON.stringify({ command: "/owner/agent", cwd: ".", args: [] });
    writeFileSync(join(f.root, name + ".json"), custom);
    assert.throws(() => f.call({ agents: ["codex"] }), /customized/);
    assert.equal(readFileSync(join(f.root, name + ".json"), "utf8"), custom);
  }
});
test("first-install failure restores migrated profiles, pointer and shim", (t) => {
  const f = fixture(t), profile = JSON.parse(readFileSync(join(f.bundle, "profiles.json"), "utf8")).profiles.find((p: any) => p.id === "codex");
  const legacy = JSON.stringify({ command: join(f.base, "current/launchers/agent/scripts/treer-agent.sh"), cwd: ".", args: profile.args });
  writeFileSync(join(f.root, "Codex + UI.json"), legacy);
  assert.throws(() => f.call({ agents: ["codex", "claude"] }, { BAD_SHOW: "Claude + UI" }), /Profile verification failed/);
  assert.deepEqual(JSON.parse(readFileSync(join(f.root, "Codex + UI.json"), "utf8")), JSON.parse(legacy));
  assert.equal(existsSync(join(f.root, "Claude + UI.json")), false);
  assert.equal(existsSync(join(f.base, "current.json")), false);
  assert.equal(existsSync(join(f.base, "profiles-receipt.json")), false);
  assert.equal(existsSync(join(f.root, ".local/bin/treer-agent")), false);
});
test("capability disagreement, unknown old Controller and rollback incompatibility fail before mutation", (t) => {
  const f = fixture(t);
  assert.throws(() => f.call({ controller: { ...f.request.controller, capabilities: [] } }), /lacks launcher/);
  assert.equal(existsSync(join(f.root, "calls")), false);
  f.call(); assert.throws(() => f.call({ operation: "rollback", rollback_version: f.request.version, controller: { ...f.request.controller, features: [] } }), /lacks launcher/);
  assert.equal(compatibility({}, null).status, "unknown");
  assert.equal(f.call({ operation: "status", controller: null }).versions[0].compatibility.status, "unknown");
});
test("security epoch blocks unsafe rollback even when old capabilities remain supported", (t) => {
  const f = fixture(t); f.call(); const previous = f.request.version;
  const contractPath = join(f.bundle, "interface-contract.json"), contract = JSON.parse(readFileSync(contractPath, "utf8")); contract.launcher_security_epoch = 2;
  writeFileSync(contractPath, JSON.stringify(contract));
  const manifestPath = join(f.bundle, "bundle-manifest.json"), manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  const entry = manifest.files.find((file: any) => file.path === "interface-contract.json"), bytes = readFileSync(contractPath); entry.size = bytes.length; entry.sha256 = digest(bytes);
  writeFileSync(manifestPath, JSON.stringify(manifest)); f.request.version = `sha256-${digest(readFileSync(manifestPath))}`;
  f.call({ operation: "update", controller: { ...f.request.controller, minimum_launcher_security_epoch: 2 } });
  assert.throws(() => f.call({ operation: "rollback", rollback_version: previous, controller: { ...f.request.controller, minimum_launcher_security_epoch: 2 } }), /security floor/);
  assert.equal(f.call({ operation: "status" }).current, f.request.version);
});
test("owner profile customizations are preserved and tampered stable entry is detected and repaired", (t) => {
  const f = fixture(t); f.call();
  const profilePath = join(f.root, "Codex + UI.json"), original = readFileSync(profilePath, "utf8"), custom = JSON.parse(original);
  custom.args.push("--sandbox", "owner choice"); writeFileSync(profilePath, JSON.stringify(custom)); revise(f);
  assert.throws(() => f.call({ operation: "update" }), /customized/);
  assert.deepEqual(JSON.parse(readFileSync(profilePath, "utf8")), custom);
  writeFileSync(profilePath, original);
  const runner = join(f.base, "run.mjs"); writeFileSync(runner, "tampered entry");
  assert.equal(f.call({ operation: "status" }).entry_integrity, "mismatch");
  f.call({ operation: "update" }); assert.equal(f.call({ operation: "status" }).entry_integrity, "valid");
});
test("development source refuses implicit network and verifies source on reinstall", (t) => {
  const f = fixture(t); rmSync(join(f.bundle, "runtime.mjs"));
  for (const name of ["src", "package.json", "package-lock.json"]) cpSync(join(launcher, name), join(f.bundle, name), { recursive: true });
  const manifestPath = join(f.bundle, "bundle-manifest.json"), manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  manifest.runtime_ready = false; manifest.files = [];
  function visit(directory: string, prefix = "") {
    for (const name of readdirSync(directory)) {
      const path = join(directory, name), relative = prefix + name;
      if (lstatSync(path).isDirectory()) visit(path, relative + "/");
      else if (relative !== "bundle-manifest.json") { const bytes = readFileSync(path); manifest.files.push({ path: relative, size: bytes.length, sha256: digest(bytes), executable: Boolean(lstatSync(path).mode & 0o111) }); }
    }
  }
  visit(f.bundle); writeFileSync(manifestPath, JSON.stringify(manifest)); f.request.version = `sha256-${digest(readFileSync(manifestPath))}`;
  assert.throws(() => f.call(), /not runtime-ready/);
  assert.equal(existsSync(join(f.root, "calls")), false);
  const npm = join(f.root, "npm.cjs");
  writeFileSync(npm, `const fs=require('node:fs');if(!process.argv.includes('--ignore-scripts'))throw Error('lifecycle scripts must be disabled');fs.mkdirSync('node_modules/tsx/dist',{recursive:true});fs.mkdirSync('node_modules/ws',{recursive:true});fs.writeFileSync('node_modules/tsx/dist/loader.mjs','');fs.writeFileSync('node_modules/ws/package.json','{}');`);
  assert.equal(f.call({ allow_network: true, npm_cli: npm }).portable, false);
  assert.equal(f.call({ operation: "status" }).versions[0].compatibility.status, "compatible");
  writeFileSync(join(f.base, f.request.version, "src/index.ts"), "tampered");
  assert.throws(() => f.call({ allow_network: true, npm_cli: npm }), /integrity mismatch/);
});
test("bundle integrity, path traversal, symlink escape and concurrent locks fail closed", (t) => {
  const f = fixture(t); writeFileSync(join(f.bundle, "runtime.mjs"), "unverified code");
  assert.throws(() => f.call(), /integrity mismatch/); assert.equal(existsSync(join(f.root, "calls")), false);
  const g = fixture(t); symlinkSync(g.root, join(g.machine, ".treer"), "dir"); assert.throws(() => g.call(), /link/);
  const alias = fixture(t), linkedRoot = join(alias.root, "linked-machine");
  symlinkSync(alias.machine, linkedRoot, "dir");
  assert.throws(() => alias.call({ root: linkedRoot }), /Machine root must be a physical path/);
  const cwdEscape = fixture(t);
  symlinkSync(cwdEscape.root, join(cwdEscape.machine, "outside-cwd"), "dir");
  assert.throws(() => cwdEscape.call({ cwd: "outside-cwd" }), /Profile cwd must stay inside machine root/);
  assert.equal(existsSync(join(cwdEscape.root, "calls")), false);
  const h = fixture(t); h.call(); mkdirSync(join(h.base, ".install-lock")); assert.throws(() => h.call(), /owns the lock/);
  const k = fixture(t); const manifest = JSON.parse(readFileSync(join(k.bundle, "bundle-manifest.json"), "utf8")); manifest.files[0].path = "../escape";
  writeFileSync(join(k.bundle, "bundle-manifest.json"), JSON.stringify(manifest)); k.request.version = `sha256-${digest(readFileSync(join(k.bundle, "bundle-manifest.json")))}`;
  assert.throws(() => k.call(), /manifest entry/);
  const m = fixture(t); writeFileSync(join(m.bundle, "extra.js"), "not listed"); assert.throws(() => m.call(), /integrity mismatch/);
});
test("portable runtime and Node entry register the same interface contract without shell or node_modules", async (t) => {
  const f = fixture(t); f.call();
  const child = spawn(process.execPath, [join(f.base, "run.mjs"), "--runtime", "node", "--harness", "codex"], { cwd: f.machine, env: hermeticEnv({ FIXTURE_ROOT: f.root, PATH: dirname(process.execPath), CODEX_BIN: join(launcher, "tests/fake-codex.mjs"), AGENT_UI_MESSAGE_WATCH: "0" }), stdio: "pipe" });
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
