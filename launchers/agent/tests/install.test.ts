import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { hermeticEnv } from "./hermetic.mjs";

const launcher = resolve(import.meta.dirname, "..");
const mockTreer = `#!/usr/bin/env node
const fs = require('node:fs'), path = require('node:path');
const root = process.env.FIXTURE_ROOT, args = process.argv.slice(2);
fs.appendFileSync(path.join(root, 'calls'), JSON.stringify(args)+'\\n');
if (args[0] === 'whoami') { console.log(JSON.stringify({machine:{root:path.join(root,'machine root')},agent:{cwd:'.'}})); process.exit(); }
if (args.slice(0,3).join(' ') !== 'agent admin profile') throw Error('Unexpected Treer mutation: '+args);
const [action, name] = args.slice(3), file = path.join(root, name+'.json');
if (action === 'show') {
 if (!fs.existsSync(file)) { console.error('profile_not_found'); process.exit(1); }
 const value = JSON.parse(fs.readFileSync(file));
 if ((process.env.BAD_SHOW || process.env.BAD_SHOW_PROFILE===name) && fs.existsSync(path.join(root,'modified-'+name))) value.args=[];
 console.log(JSON.stringify(value));
} else if (action === 'create' || action === 'update') {
 const cwd=args[args.indexOf('--cwd')+1];
 const command=action==='create'?args[args.indexOf('--cwd')+2]:args[args.indexOf('--command')+1];
 const tail=action==='create'?args.slice(args.indexOf('--')+1):args.flatMap((arg,i)=>arg==='--arg'?[args[i+1]]:[]);
 fs.writeFileSync(file,JSON.stringify({cwd,command,args:tail}));
 if (process.env.BAD_SHOW || process.env.BAD_SHOW_PROFILE===name) fs.writeFileSync(path.join(root,'modified-'+name),'1');
} else if (action === 'delete') {
 fs.unlinkSync(file);
} else throw Error('Unexpected action: '+action);
`;
const mockNpm = `#!/usr/bin/env node
const fs=require('node:fs');
if (process.argv.slice(2).join(' ') !== 'ci --omit=dev --no-audit --no-fund') throw Error('npm must use lockfile and production dependencies');
if (process.env.NPM_FAIL) process.exit(12);
fs.mkdirSync('node_modules/.bin',{recursive:true}); fs.mkdirSync('node_modules/ws',{recursive:true});
fs.writeFileSync('node_modules/.bin/tsx','#!/bin/sh\\nexit 0\\n',{mode:0o755}); fs.writeFileSync('node_modules/ws/package.json','{}');
`;

function fixture() {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "agent-install-")));
  const source = join(root, "source"), bin = join(root, "bin"), machine = join(root, "machine root");
  mkdirSync(source); mkdirSync(bin); mkdirSync(machine);
  cpSync(launcher, join(source, "launchers/agent"), { recursive: true, filter: (path) => !path.split(/[\\/]/).includes("node_modules") });
  writeFileSync(join(bin, "treer"), mockTreer, { mode: 0o755 });
  writeFileSync(join(bin, "npm"), mockNpm, { mode: 0o755 });
  const env = hermeticEnv({ HOME: root, LOCALAPPDATA: root, FIXTURE_ROOT: root, PATH: `${bin}:${process.env.PATH}`, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" });
  const git = (...args: string[]) => execFileSync("git", ["-C", source, ...args], { env, encoding: "utf8" }).trim();
  git("init", "-q"); git("config", "user.name", "Fixture"); git("config", "user.email", "test@example.invalid");
  const commit = () => { git("add", "."); git("commit", "-qm", "fixture"); return git("rev-parse", "HEAD"); };
  const first = commit();
  const install = (args = ["--source", source], extra = {}) => execFileSync(process.execPath,
    [join(launcher, "scripts/install.mjs"), ...args, "--agent", "codex"], { env: { ...env, ...extra }, encoding: "utf8", stdio: "pipe" });
  return { root, source, machine, first, commit, install, base: join(machine, ".treer/launchers/agent") };
}

test("installer uses immutable committed source, verifies profiles, and updates current without touching previous versions", (t) => {
  const f = fixture(); t.after(() => rmSync(f.root, { recursive: true, force: true }));
  f.install();
  const firstRoot = realpathSync(join(f.base, "current"));
  assert.equal(firstRoot, join(f.base, f.first));
  const profile = JSON.parse(readFileSync(join(f.root, "Codex + UI.json"), "utf8"));
  assert.deepEqual(profile, { cwd: ".", command: "treer-agent", args: ["--harness", "codex"] });
  f.install();
  writeFileSync(join(f.source, "launchers/agent/dirty"), "not committed");
  f.install();
  assert.equal(existsSync(join(firstRoot, "launchers/agent/dirty")), false);
  const second = f.commit(); f.install();
  assert.equal(realpathSync(join(f.base, "current")), join(f.base, second));
  assert.ok(existsSync(join(firstRoot, "launchers/agent/node_modules/.bin/tsx")));
  const calls = readFileSync(join(f.root, "calls"), "utf8").trim().split("\n").map((line) => JSON.parse(line));
  assert.ok(calls.every((args) => args[0] === "whoami" || args.slice(0, 3).join(" ") === "agent admin profile"));
  assert.ok(calls.filter((args) => args[3] === "show").length >= 8);
});

test("installer sparse-fetches a full commit from a local git repository", (t) => {
  const f = fixture(); t.after(() => rmSync(f.root, { recursive: true, force: true }));
  f.install(["--repository", f.source, "--ref", f.first]);
  assert.equal(realpathSync(join(f.base, "current")), join(f.base, f.first));
  assert.equal(existsSync(join(f.base, f.first, ".git")), false);
});

test("npm failure and profile verification failure leave current on the previous working version", (t) => {
  const f = fixture(); t.after(() => rmSync(f.root, { recursive: true, force: true }));
  f.install();
  writeFileSync(join(f.source, "launchers/agent/revision"), "next"); f.commit();
  assert.throws(() => f.install(undefined, { NPM_FAIL: "1" }));
  assert.equal(realpathSync(join(f.base, "current")), join(f.base, f.first));
  assert.throws(() => f.install(undefined, { BAD_SHOW: "1" }), /Profile verification failed/);
  assert.equal(realpathSync(join(f.base, "current")), join(f.base, f.first));
  assert.equal(existsSync(join(f.base, ".install-lock")), false);
  assert.deepEqual(JSON.parse(readFileSync(join(f.root, "Codex + UI.json"), "utf8")).args, ["--harness", "codex"]);
});

test("first-install verification failure removes the newly created profile before removing current", (t) => {
  const f = fixture(); t.after(() => rmSync(f.root, { recursive: true, force: true }));
  assert.throws(() => f.install(undefined, { BAD_SHOW: "1" }), /Profile verification failed/);
  assert.equal(existsSync(join(f.base, "current")), false);
  assert.equal(existsSync(join(f.root, "Codex + UI.json")), false);
});

test("failure verifying a second profile restores earlier edits and removes new profiles", (t) => {
  const f = fixture(); t.after(() => rmSync(f.root, { recursive: true, force: true }));
  f.install();
  const previous = readFileSync(join(f.root, "Codex + UI.json"), "utf8");
  const manifestPath = join(f.source, "launchers/agent/profiles.json");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  manifest.profiles.find((p: { id: string }) => p.id === "codex").description = "Updated Codex";
  writeFileSync(manifestPath, JSON.stringify(manifest)); f.commit();
  assert.throws(() => f.install(["--source", f.source, "--agent", "codex", "--agent", "claude"], { BAD_SHOW_PROFILE: "Claude + UI" }), /Profile verification failed/);
  assert.equal(realpathSync(join(f.base, "current")), join(f.base, f.first));
  assert.equal(readFileSync(join(f.root, "Codex + UI.json"), "utf8"), previous);
  assert.equal(existsSync(join(f.root, "Claude + UI.json")), false);
});

test("invalid provider and cwd outside machine root fail before installing", (t) => {
  const f = fixture(); t.after(() => rmSync(f.root, { recursive: true, force: true }));
  assert.throws(() => f.install(["--source", f.source, "--agent", "pi"]), /Unknown provider/);
  assert.throws(() => f.install(["--source", f.source, "--cwd", ".."]), /inside machine.root/);
  symlinkSync(f.root, join(f.machine, "outside-cwd"), "dir");
  assert.throws(() => f.install(["--source", f.source, "--cwd", "outside-cwd"]), /inside machine.root/);
  assert.equal(existsSync(f.base), false);
});
