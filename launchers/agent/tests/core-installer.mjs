// Opt-in real Controller binary + sealed fake enrolled CLI; never starts Agents.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { hermeticEnv } from "./hermetic.mjs";
const binary = process.argv[2];
if (!isAbsolute(binary || "")) throw new Error("Usage: node tests/core-installer.mjs /absolute/path/to/treer-agent-server");
if (process.platform === "win32") throw new Error("This Unix shebang CLI fixture is not a Windows native acceptance test");
const launcher = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const parent = resolve(launcher, "../../.treer"); mkdirSync(parent, { recursive: true, mode: 0o700 });
const root = realpathSync(mkdtempSync(join(parent, "core-install-fixture-"))), machine = join(root, "machine"), cli = join(root, "treer"); mkdirSync(machine, { mode: 0o700 });
const contract = JSON.parse(readFileSync(join(launcher, "interface-contract.json"), "utf8"));
writeFileSync(join(root, "contract.json"), JSON.stringify({ git_commit: "a".repeat(40), protocol: contract.protocol, capabilities: contract.capabilities, features: contract.required_features,
  minimum_launcher_security_epoch: 1, machine: { root: machine, server_id: "srv_fixture", workspace_id: "ws_fixture" } }), { mode: 0o600 });
writeFileSync(cli, `#!${process.execPath}
const fs=require('node:fs'),path=require('node:path'),root=process.env.FIXTURE_ROOT,args=process.argv.slice(2);
if(args.join(' ')==='interface contract'){console.log(fs.readFileSync(path.join(root,'contract.json'),'utf8'));process.exit()}
if(args.join(' ')==='status'){console.log('{"agents":[]}');process.exit()}
if(args.slice(0,3).join(' ')!=='agent admin profile')throw Error('Unexpected mutation');
const [action,name]=args.slice(3),file=path.join(root,name+'.json');
if(action==='show'){if(!fs.existsSync(file)){console.error('profile_not_found');process.exit(1)}console.log(fs.readFileSync(file,'utf8'))}
else if(action==='create'||action==='update'){const i=args.indexOf('--cwd'),command=action==='create'?args[i+2]:args[args.indexOf('--command')+1],tail=action==='create'?args.slice(args.indexOf('--')+1):args.flatMap((v,i)=>v==='--arg'?[args[i+1]]:[]);fs.writeFileSync(file,JSON.stringify({cwd:args[i+1],command,args:tail}))}
else if(action==='delete')fs.unlinkSync(file);else throw Error('Unexpected mutation');
`, { mode: 0o700 });
const env = hermeticEnv({ HOME: root, FIXTURE_ROOT: root, PATH: dirname(process.execPath) });
const run = (...args) => JSON.parse(execFileSync(binary, ["launcher", "--node", process.execPath, "--treer", cli, ...args], { env, encoding: "utf8", stdio: "pipe" }));
try {
  const installed = run("install"); assert.equal(installed.portable, true); assert.equal(installed.profiles.length, 6);
  const again = run("install"); assert.equal(again.version, installed.version);
  const updated = run("update"); assert.equal(updated.version, installed.version);
  const status = run("status"); assert.equal(status.current, installed.version); assert.equal(status.versions[0].compatibility.status, "compatible");
  const rolledBack = run("rollback", installed.version); assert.equal(rolledBack.version, installed.version);
  console.log(JSON.stringify({ real_controller_cli: true, install: "passed", idempotence: "passed", update: "passed", status: "passed", rollback: "passed", profiles: 6, credentials: "sealed fake CLI only", agents_started: 0 }));
} finally { rmSync(root, { recursive: true, force: true }); }
