import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { hermeticEnv } from "./hermetic.mjs";
import { nativeBindingFixture } from "./native-binding-fixture.mjs";
import { digest } from "../scripts/bundle-contract.mjs";

const source = resolve(import.meta.dirname, ".."), temporary = () => realpathSync.native(mkdtempSync(join(tmpdir(), "launcher-update-")));
const prepared = temporary(), bindingRoot = temporary();
const binding = nativeBindingFixture(bindingRoot, join(source, "public"));
execFileSync(process.execPath, [join(source, "scripts/prepare-bundle.mjs"), "--output", prepared, "--source-commit", "a".repeat(40), "--native-manifest", binding.path, "--native-sha256", binding.sha256], { env: hermeticEnv(), stdio: "pipe" });
test.after(() => { rmSync(prepared, { recursive: true, force: true }); rmSync(bindingRoot, { recursive: true, force: true }); });

const cli = `import fs from 'node:fs';import path from 'node:path';
const root=process.env.UPDATE_FIXTURE,args=process.argv.slice(2),file=path.join(root,'db.json'),db=JSON.parse(fs.readFileSync(file));
db.calls.push(args);fs.writeFileSync(file,JSON.stringify(db));const out=v=>console.log(JSON.stringify(v));
if(args.join(' ')==='interface contract')out(db.controller);
else if(args.slice(0,3).join(' ')==='agent admin profile'){
const [action,name]=args.slice(3);if(action==='show'){if(!db.profiles[name]){console.error('profile_not_found');process.exit(1)}out(db.profiles[name]);}
else if(action==='create'||action==='update'){const i=args.indexOf('--cwd');db.profiles[name]={command:action==='create'?args[i+2]:args[args.indexOf('--command')+1],cwd:args[i+1],args:action==='create'?args.slice(args.indexOf('--')+1):args.flatMap((v,i)=>v==='--arg'?[args[i+1]]:[])};fs.writeFileSync(file,JSON.stringify(db));}
else if(action==='delete'){delete db.profiles[name];fs.writeFileSync(file,JSON.stringify(db));}else throw Error('unexpected profile');
}else throw Error('unexpected lifecycle/provider operation');`;

function fixture(t: any) {
  const root = temporary(), machine = join(root, "machine"), bundle = join(root, "candidate"), treer = join(root, "treer.mjs"), file = join(root, "db.json");
  mkdirSync(machine); cpSync(prepared, bundle, { recursive: true }); writeFileSync(treer, cli);
  const contract = JSON.parse(readFileSync(join(bundle, "interface-contract.json"), "utf8"));
  writeFileSync(file, JSON.stringify({ calls: [], profiles: {}, controller: { ...contract, features: contract.required_features, minimum_launcher_security_epoch: 1, machine: { root: machine, server_id: "srv_fixture" } } }));
  const env = hermeticEnv({ HOME: root, LOCALAPPDATA: root, UPDATE_FIXTURE: root, PATH: dirname(process.execPath) });
  const run = (entry: string, args: string[], extra = {}) => spawnSync(process.execPath, [entry, ...args], { encoding: "utf8", env: { ...env, ...extra }, timeout: 30000 });
  const hash = () => digest(readFileSync(join(bundle, "bundle-manifest.json")));
  const install = () => run(join(source, "scripts/bundle-launcher.mjs"), ["install", "--bundle", bundle, "--sha256", hash(), "--treer", treer]);
  const base = join(machine, ".treer/launchers/agent"), runner = join(base, "run.mjs");
  const update = (...args: string[]) => run(runner, ["update", "--bundle", bundle, "--sha256", hash(), ...args]);
  const changeManifest = (change: (m: any) => void) => { const path = join(bundle, "bundle-manifest.json"), m = JSON.parse(readFileSync(path, "utf8")); change(m); writeFileSync(path, JSON.stringify(m)); };
  const db = () => JSON.parse(readFileSync(file, "utf8"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return { root, machine, bundle, treer, file, base, runner, run, hash, install, update, changeManifest, db, env };
}

test("installed real entry discovers update/help before provider args and performs the existing Rust bundle transaction", t => {
  const f = fixture(t); assert.equal(f.install().status, 0);
  const initial = f.hash(), oldRuntime = readFileSync(join(f.base, `sha256-${initial}`, "runtime.mjs"));
  const help = f.run(f.runner, ["--help"]); assert.equal(help.status, 0, help.stderr); assert.match(help.stdout, /treer-agent update/);
  assert.equal(f.run(f.runner, ["update", "--help"]).status, 0);
  f.changeManifest(m => m.source_commit = "b".repeat(40));
  const result = f.update(); assert.equal(result.status, 0, result.stderr);
  const output = JSON.parse(result.stdout);
  assert.equal(output.current, `sha256-${initial}`); assert.equal(output.target, `sha256-${f.hash()}`);
  assert.equal(output.runtime.default, "rust"); assert.equal(output.changed, true); assert.equal(output.running_agents, "unchanged");
  assert.match(result.stderr, /verified-candidate/);
  assert.equal(JSON.parse(readFileSync(join(f.base, "current.json"), "utf8")).version, output.target);
  assert.deepEqual(readFileSync(join(f.base, `sha256-${initial}`, "runtime.mjs")), oldRuntime);
  assert.equal(JSON.parse(f.update().stdout).changed, false);
  assert.deepEqual(f.db().profiles["Codex + UI"].args, ["--harness", "codex"]);
  assert.deepEqual(f.db().profiles["Claude + UI"].args, ["--harness", "claude"]);
  assert.deepEqual(f.db().calls.filter((a: string[]) => a[0] !== "interface" && a.slice(0, 3).join(" ") !== "agent admin profile"), []);
  // POSIX real installed shell shim, not only the module entry. Windows' PE
  // install entry requires its native runner and is reported separately.
  if (process.platform !== "win32") {
    const command = spawnSync(join(f.root, ".local/bin/treer-agent"), ["update", "--help"], { encoding: "utf8", env: f.env });
    assert.equal(command.status, 0, command.stderr); assert.match(command.stdout, /require-runtime rust/);
  }
});

test("bad hash, Node/unconfigured candidates, argument mixing and customized profiles leave the usable entry intact", t => {
  const f = fixture(t); assert.equal(f.install().status, 0);
  const pointer = readFileSync(join(f.base, "current.json"));
  assert.notEqual(f.update("--sha256", "f".repeat(64)).status, 0); // duplicate selection refused
  assert.notEqual(f.run(f.runner, ["update", "--bundle", f.bundle, "--sha256", "f".repeat(64)]).status, 0);
  assert.notEqual(f.update("--harness", "codex").status, 0);
  const original = readFileSync(join(f.bundle, "bundle-manifest.json"));
  f.changeManifest(m => m.files = m.files.filter((p: any) => p.path !== "native-runtime.json"));
  rmSync(join(f.bundle, "native-runtime.json"));
  assert.match(f.update().stderr, /Node\/unconfigured/);
  // Installer-level defense applies independently of the convenience frontend.
  const direct = f.run(join(source, "scripts/bundle-launcher.mjs"), ["update", "--treer", f.treer, "--bundle", f.bundle, "--sha256", f.hash()]);
  assert.notEqual(direct.status, 0); assert.match(direct.stderr, /native Rust default/);
  cpSync(join(prepared, "native-runtime.json"), join(f.bundle, "native-runtime.json")); writeFileSync(join(f.bundle, "bundle-manifest.json"), original);
  f.changeManifest(m => m.source_commit = "c".repeat(40));
  const db = f.db(); db.profiles["Codex + UI"].command = "/owner/custom"; writeFileSync(f.file, JSON.stringify(db));
  assert.notEqual(f.update().status, 0); assert.deepEqual(readFileSync(join(f.base, "current.json")), pointer);
  assert.equal(f.run(f.runner, ["--help"]).status, 0);
});

test("guarded local Controller path passes the Rust requirement once and never falls back after refusal", t => {
  const f = fixture(t); assert.equal(f.install().status, 0);
  const controller = join(f.root, "controller.mjs"), calls = join(f.root, "bridge.json");
  writeFileSync(calls, "[]");
  writeFileSync(controller, `import fs from 'node:fs';const args=process.argv.slice(2),file=${JSON.stringify(calls)},calls=JSON.parse(fs.readFileSync(file));calls.push(args);fs.writeFileSync(file,JSON.stringify(calls));
if(args.at(-1)==='status')console.log(JSON.stringify({current:'sha256-'+ 'a'.repeat(64)}));
else if(process.env.REFUSE_UPDATE){console.error('unrecognized flag --require-runtime secret-do-not-relay');process.exit(2)}
else {if(args.slice(-3).join(' ')!=='update --require-runtime rust')throw Error('unguarded');console.log(JSON.stringify({version:'sha256-'+'b'.repeat(64),runtime:{default:'rust',codex:'rust',claude:'unavailable-provider'}}));}`);
  const run = (extra = {}) => f.run(f.runner, ["update", "--controller", controller], extra);
  const result = run(); assert.equal(result.status, 0, result.stderr); assert.equal(JSON.parse(result.stdout).runtime.default, "rust");
  const refused = run({ REFUSE_UPDATE: "1" }); assert.notEqual(refused.status, 0); assert.match(refused.stderr, /No retry or unguarded fallback/); assert.doesNotMatch(refused.stderr, /secret-do-not-relay/);
  const seen = JSON.parse(readFileSync(calls, "utf8")); assert.equal(seen.filter((a: string[]) => a.includes("update")).length, 2);
  for (const a of seen.filter((a: string[]) => a.includes("update"))) assert.deepEqual(a.slice(-3), ["update", "--require-runtime", "rust"]);
  assert.equal(existsSync(join(f.base, ".install-lock")), false);
});

test("provider availability, install lock and installed-root binding are enforced before publication", t => {
  const f = fixture(t); assert.equal(f.install().status, 0);
  const nativePath = join(f.bundle, "native-runtime.json"), metadata = JSON.parse(readFileSync(nativePath, "utf8"));
  const saveNative = () => {
    const bytes = Buffer.from(JSON.stringify(metadata)); writeFileSync(nativePath, bytes);
    f.changeManifest(m => { const row = m.files.find((p: any) => p.path === "native-runtime.json"); row.size = bytes.length; row.sha256 = digest(bytes); });
  };
  delete metadata.tools.codex; saveNative();
  const claudeOnly = f.update(); assert.equal(claudeOnly.status, 0, claudeOnly.stderr);
  assert.equal(JSON.parse(claudeOnly.stdout).runtime.codex, "unavailable-provider");
  const pointer = readFileSync(join(f.base, "current.json"));
  delete metadata.tools.claude; saveNative();
  assert.match(f.update().stderr, /available Codex or Claude/);
  metadata.tools.claude = binding.value.tools.claude; saveNative();
  mkdirSync(join(f.base, ".install-lock"));
  assert.match(f.update().stderr, /installation owns the lock/);
  rmSync(join(f.base, ".install-lock"), { recursive: true });
  assert.match(f.update("--root", f.root).stderr, /differs from this installed entry/);
  const db = f.db(); db.controller.machine.root = f.root; writeFileSync(f.file, JSON.stringify(db));
  assert.match(f.run(f.runner, ["update", "--controller", f.treer]).stderr, /Controller root differs/);
  assert.deepEqual(readFileSync(join(f.base, "current.json")), pointer);
});
