import assert from "node:assert/strict";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { createServer } from "node:http";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, truncateSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { hermeticEnv } from "./hermetic.mjs";
import { nativeBindingFixture } from "./native-binding-fixture.mjs";
import { digest } from "../scripts/bundle-contract.mjs";
import { updateExecutable } from "../scripts/update.mjs";
import { releasePlatform } from "../scripts/latest-download.mjs";

const source = resolve(import.meta.dirname, ".."), temporary = () => realpathSync.native(mkdtempSync(join(tmpdir(), "launcher-update-")));
const prepared = temporary(), bindingRoot = temporary();
const binding = nativeBindingFixture(bindingRoot, join(source, "public"), process.platform === "win32" ? process.execPath : "/usr/bin/true");
execFileSync(process.execPath, [join(source, "scripts/prepare-bundle.mjs"), "--output", prepared, "--source-commit", "a".repeat(40), "--native-manifest", binding.path, "--native-sha256", binding.sha256], { env: hermeticEnv(), stdio: "pipe" });
test.after(() => { rmSync(prepared, { recursive: true, force: true }); rmSync(bindingRoot, { recursive: true, force: true }); });

test("local executable selectors resolve installed links without applying bundle-member memory/size limits", t => {
  const root = temporary(); t.after(() => rmSync(root, { recursive: true, force: true }));
  const executable = join(root, "controller"), link = join(root, "controller-link");
  writeFileSync(executable, "x"); truncateSync(executable, 40 * 1024 * 1024);
  assert.equal(updateExecutable(executable, "Controller"), executable);
  if (process.platform !== "win32") {
    symlinkSync(executable, link); assert.equal(updateExecutable(link, "Controller"), executable);
  }
  assert.throws(() => updateExecutable(root, "Controller"), /Controller executable selector/);
  assert.throws(() => updateExecutable(join(root, "missing"), "Treer CLI"), /Treer CLI executable selector/);
});

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
  if (process.platform !== "win32") {
    const controllerLink = join(f.root, "controller-link.mjs"); symlinkSync(controller, controllerLink);
    const linked = f.run(f.runner, ["update", "--controller", controllerLink]); assert.equal(linked.status, 0, linked.stderr);
    writeFileSync(calls, "[]");
  }
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

test("actual installed latest entry freezes one HTTP channel, installs verified native payload and same version performs no writes", async t => {
  if (process.platform === "win32") { t.skip("Windows native test image exceeds the bounded release fixture; requires a small PE control image"); return; }
  const f = fixture(t); assert.equal(f.install().status, 0);
  const sourceCommit = "d".repeat(40), platform = releasePlatform();
  const original = JSON.parse(readFileSync(join(f.bundle, "bundle-manifest.json"), "utf8"));
  const payload = { ...original, source_commit: sourceCommit, files: original.files.filter((file: any) => file.path !== "native-runtime.json") };
  const manifest = Buffer.from(JSON.stringify(payload));
  const encoded = (path: string, data: Buffer, executable = false) => ({ path, size: data.length, sha256: digest(data), executable, data_base64: data.toString("base64") });
  const native = readFileSync(binding.value.executable.path);
  const value = { schema: "treer.launcher-package/v1", source_commit: sourceCommit, platform, payload_manifest_sha256: digest(manifest), native: { path: "native/treer-agent", source_commit: binding.value.source_commit, tools: binding.value.tools }, files: [encoded("payload/bundle-manifest.json", manifest), ...payload.files.map((file: any) => encoded(`payload/${file.path}`, readFileSync(join(f.bundle, file.path)), file.executable)), encoded("native/treer-agent", native, true)] };
  let packageBytes = Buffer.from(JSON.stringify(value)), mode = "ok", channelReads = 0, packageReads = 0;
  const http = createServer((request, response) => {
    if (request.url === "/latest") {
      channelReads++;
      const row = { package_url: `${base}/package`, package_sha256: mode === "checksum" ? "f".repeat(64) : digest(packageBytes), package_size: packageBytes.length };
      response.end(JSON.stringify({ schema: "treer.launcher-latest/v1", source_commit: sourceCommit, platforms: mode === "platform" ? {} : { [platform]: row } }));
    } else {
      packageReads++;
      if (mode === "cut") { response.writeHead(200, { "Content-Length": packageBytes.length }); response.flushHeaders(); response.write(packageBytes.subarray(0, 20)); setImmediate(() => response.destroy()); }
      else response.end(packageBytes);
    }
  });
  await new Promise<void>(resolve => http.listen(0, "127.0.0.1", resolve));
  t.after(() => { http.closeAllConnections(); http.close(); });
  const base = `http://127.0.0.1:${(http.address() as any).port}`;
  const run = () => new Promise<{ status: number | null, stdout: string, stderr: string }>((resolve, reject) => {
    const child = spawn(process.execPath, [f.runner, "update", "--latest", "--release-url", `${base}/latest`], { env: f.env });
    let stdout = "", stderr = ""; child.stdout.on("data", data => stdout += data); child.stderr.on("data", data => stderr += data);
    const timer = setTimeout(() => { child.kill(); reject(new Error("latest entry fixture deadline")); }, 20000);
    child.on("error", reject); child.on("close", status => { clearTimeout(timer); resolve({ status, stdout, stderr }); });
  });
  const initial = readFileSync(join(f.base, "current.json"));
  for (const failure of ["checksum", "cut", "platform", "tools", "closure"]) {
    mode = failure;
    const candidate = JSON.parse(JSON.stringify(value));
    if (failure === "tools") candidate.native.tools.codex.path = "/different/provider";
    if (failure === "closure") candidate.files.push(encoded("payload/extra.mjs", Buffer.from("unlisted")));
    packageBytes = Buffer.from(JSON.stringify(candidate));
    const result = await run(); assert.notEqual(result.status, 0, failure); assert.deepEqual(readFileSync(join(f.base, "current.json")), initial);
    assert.equal(f.run(f.runner, ["--help"]).status, 0);
  }
  mode = "ok"; packageBytes = Buffer.from(JSON.stringify(value));
  const result = await run(); assert.equal(result.status, 0, result.stderr);
  const output = JSON.parse(result.stdout); assert.equal(output.changed, true); assert.equal(output.release_source, sourceCommit); assert.equal(output.runtime.source_commit, binding.value.source_commit); assert.equal(output.running_agents, "unchanged");
  const after = readFileSync(join(f.base, "current.json")), count = packageReads, profiles = JSON.stringify(f.db().profiles);
  const repeat = await run(); assert.equal(repeat.status, 0, repeat.stderr); assert.equal(JSON.parse(repeat.stdout).status, "unchanged");
  assert.equal(packageReads, count); assert.equal(channelReads, 7); assert.deepEqual(readFileSync(join(f.base, "current.json")), after); assert.equal(JSON.stringify(f.db().profiles), profiles);
  assert.deepEqual(f.db().calls.filter((args: string[]) => args[0] !== "interface" && args.slice(0, 3).join(" ") !== "agent admin profile"), []);
});
