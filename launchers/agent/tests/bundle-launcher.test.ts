import assert from "node:assert/strict";
import { spawn, execFileSync, type ChildProcess } from "node:child_process";
import { cpSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, rmdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import { hermeticEnv } from "./hermetic.mjs";
import { digest } from "../scripts/bundle-contract.mjs";

const launcher = resolve(import.meta.dirname, ".."), entry = join(launcher, "scripts/bundle-launcher.mjs");
const temp = () => realpathSync.native(mkdtempSync(join(tmpdir(), "bundle-delivery-")));
const prepared = temp();
execFileSync(process.execPath, [join(launcher, "scripts/prepare-bundle.mjs"), "--output", prepared, "--source-commit", "a".repeat(40)], { env: hermeticEnv(), stdio: "pipe" });
test.after(() => rmSync(prepared, { recursive: true, force: true }));

const fakeTreer = `import fs from 'node:fs';
const file=process.env.DELIVERY_FIXTURE, db=JSON.parse(fs.readFileSync(file,'utf8')), args=process.argv.slice(2), id=args[2]==='startup'?args[4]:args[2], agent=db.agents[id];
const save=()=>{const temporary=file+'.'+process.pid;fs.writeFileSync(temporary,JSON.stringify(db));fs.renameSync(temporary,file);}, out=v=>console.log(JSON.stringify(v));
db.calls.push(args);save();
if(args[0]==='interface')out(db.controller);
else if(args[0]==='message'){const key=args[args.indexOf('--idempotency-key')+1];db.messages[key]=true;save();out({accepted:true});}
else if(args[1]==='show')out(agent);
else if(args[2]==='startup'&&args[3]==='show')out(db.startups[id]);
else if(args[2]==='startup'&&args[3]==='set'){const end=args.indexOf('--');db.startups[id]={...db.startups[id],cwd:args[args.indexOf('--cwd')+1],command:args[end+1],args:args.slice(end+2)};save();out(db.startups[id]);}
else if(args[2]==='restart'){const item=db.agents[args[3]];item.run_generation++;item.interface.instance_id='new-'+args[3]+'-'+item.run_generation;if(db.crashRestart){db.crashRestart=false;save();process.kill(process.ppid,'SIGKILL');process.exit(0);}save();out(item);}
else throw Error('Unexpected fixture operation');
`;
const fakeHttp = `import http from 'node:http';import fs from 'node:fs';
const file=process.env.DELIVERY_FIXTURE;
const server=http.createServer((req,res)=>{const db=JSON.parse(fs.readFileSync(file,'utf8')), item=Object.values(db.agents).find(a=>a.interface.port===server.address().port)||Object.values(db.agents)[0], id=item.agent_id;res.setHeader('Content-Type','application/json');
if(req.url==='/api/state')res.end(JSON.stringify({ready:!(db.fail===id&&item.run_generation===2),harness:{id:'acp'},thread:{id:'session-'+id+(db.wrongSession===id&&item.run_generation===2?'-changed':''),cwd:db.controller.machine.root}}));
else if(req.url==='/v1/manifest')res.end(JSON.stringify({...db.contract,instance_id:item.interface.instance_id}));
else res.end('UI');});
server.listen(0,'127.0.0.1',()=>console.log(server.address().port));
`;
async function fixture(t: { after: (fn: () => Promise<void>) => void }) {
  const root = temp(), bundle = join(root, "bundle"), machine = join(root, "machine"), treer = join(root, "treer.mjs"), file = join(root, "db.json");
  mkdirSync(machine); cpSync(prepared, bundle, { recursive: true }); writeFileSync(treer, fakeTreer);
  const contract = JSON.parse(readFileSync(join(bundle, "interface-contract.json"), "utf8"));
  const db = { contract, controller: { ...contract, git_commit: "a".repeat(40), features: contract.required_features, minimum_launcher_security_epoch: 1, machine: { root: machine, server_id: "srv_test", name: "fixture" } }, agents: {} as Record<string, any>, startups: {} as Record<string, any>, calls: [], messages: {} };
  writeFileSync(file, JSON.stringify(db));
  const children: ChildProcess[] = [];
  t.after(async () => {
    for (const child of children) if (child.exitCode === null && child.signalCode === null) { const exit = new Promise((done) => child.once("exit", done)); child.kill(); await exit; }
    rmSync(root, { recursive: true, force: true });
  });
  const env = hermeticEnv({ DELIVERY_FIXTURE: file, PATH: dirname(process.execPath) });
  const call = (args: string[]) => new Promise<{ status: number | null; output: string; error: string }>((done) => {
    const child = spawn(process.execPath, [entry, ...args, "--treer", treer, "--root", machine, "--skip-profiles"], { env, stdio: "pipe" }); children.push(child);
    let output = "", error = ""; child.stdout!.on("data", (c) => output += c); child.stderr!.on("data", (c) => error += c);
    child.on("exit", (status) => done({ status, output, error }));
  });
  const read = () => JSON.parse(readFileSync(file, "utf8"));
  const change = (fn: (db: any) => void) => { const db = read(); fn(db); writeFileSync(file, JSON.stringify(db)); };
  const hash = () => digest(readFileSync(join(bundle, "bundle-manifest.json")));
  const install = () => call(["install", "--bundle", bundle, "--sha256", hash()]);
  async function agent(id: string, name = id) {
    change((db) => {
      db.agents[id] = { agent_id: id, name, path: "/test/" + name, kind: "command", server_id: "srv_test", run_generation: 1, interface: { port: 0, instance_id: "old-" + id } };
      db.startups[id] = { agent_id: id, enabled: true, cwd: ".", command: process.execPath, args: [join(machine, ".treer/launchers/agent/run.mjs"), "--harness", "acp", "--acp-command", "/trusted/provider.mjs", "--acp-arg", "preserve-me"] };
    });
    const server = join(root, "http.mjs"); writeFileSync(server, fakeHttp);
    const child = spawn(process.execPath, [server], { env, stdio: "pipe" }); children.push(child);
    const port = await new Promise<number>((done) => child.stdout!.once("data", (c) => done(Number(String(c).trim()))));
    change((db) => { db.agents[id].interface.port = port; });
  }
  function revise() {
    const manifestPath = join(bundle, "bundle-manifest.json"), manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    const path = join(bundle, "public/index.html"), bytes = Buffer.concat([readFileSync(path), Buffer.from("\n<!--candidate-->\n")]); writeFileSync(path, bytes);
    const found = manifest.files.find((item: any) => item.path === "public/index.html"); found.sha256 = digest(bytes); found.size = bytes.length;
    writeFileSync(manifestPath, JSON.stringify(manifest));
  }
  function rollout(targets: string[], extra: string[] = []) {
    const path = join(root, "targets.json"); writeFileSync(path, JSON.stringify(targets));
    return call(["rollout", "--bundle", bundle, "--sha256", hash(), "--targets", path, "--preview", targets[0], "--rollout-id", "test", "--timeout", "1000", ...extra]);
  }
  return { root, machine, bundle, treer, file, read, change, hash, call, install, agent, revise, rollout };
}

test("local bundle install/update/status/rollback is offline, immutable and requires the exact manifest hash", async (t) => {
  const f = await fixture(t);
  assert.equal((await f.install()).status, 0);
  const first = f.hash(), base = join(f.machine, ".treer/launchers/agent"), before = readFileSync(join(base, `sha256-${first}`, "runtime.mjs"));
  f.revise(); assert.equal((await f.install()).status, 0);
  assert.equal(JSON.parse((await f.call(["status"])).output).current, `sha256-${f.hash()}`);
  assert.equal((await f.call(["rollback"])).status, 0);
  assert.equal(JSON.parse((await f.call(["status"])).output).current, `sha256-${first}`);
  assert.deepEqual(readFileSync(join(base, `sha256-${first}`, "runtime.mjs")), before);
  assert.deepEqual(f.read().calls.filter((args: string[]) => args.includes("restart")), []);
  assert.equal((await f.call(["update", "--bundle", f.bundle, "--sha256", "f".repeat(64)])).status, 1);
  if (process.platform !== "win32") assert.equal(lstatSync(base).mode & 0o777, 0o700);
});

test("rollout dry-run never changes startup/restarts; apply preserves provider/session, starts with preview alone and replays idempotently", async (t) => {
  const f = await fixture(t); await f.install();
  for (const id of ["ag_preview", "ag_second", "ag_third"]) await f.agent(id);
  f.revise(); const baseline = f.read();
  const dry = await f.rollout(["ag_preview", "ag_second", "ag_third"]);
  assert.equal(dry.status, 0, dry.error); assert.equal(JSON.parse(dry.output).dry_run, true);
  assert.deepEqual(f.read().startups, baseline.startups);
  const result = await f.rollout(["ag_preview", "ag_second", "ag_third"], ["--apply"]);
  assert.equal(result.status, 0, result.error); assert.equal(JSON.parse(result.output).status, "completed");
  const calls = f.read().calls;
  const previewMessage = calls.findIndex((args: string[]) => args[0] === "message" && args.includes("ag_preview"));
  const secondRestart = calls.findIndex((args: string[]) => args[2] === "restart" && args[3] === "ag_second");
  assert.ok(previewMessage >= 0 && secondRestart > previewMessage);
  for (const id of ["ag_preview", "ag_second", "ag_third"]) {
    assert.deepEqual(f.read().startups[id].args.slice(1, 7), ["--harness", "acp", "--acp-command", "/trusted/provider.mjs", "--acp-arg", "preserve-me"]);
    assert.ok(f.read().startups[id].args.includes(`session-${id}`));
  }
  const again = await f.rollout(["ag_preview", "ag_second", "ag_third"], ["--apply"]);
  assert.equal(again.status, 0, again.error); assert.equal(JSON.parse(again.output).replay, true);
  assert.equal(Object.keys(f.read().messages).length, 3);
  assert.equal(f.read().calls.filter((args: string[]) => args[2] === "restart").length, 3);
});

for (const failure of ["fail", "wrongSession"]) test(`preview ${failure} rolls back version/startup, never touches later Agents`, async (t) => {
  const f = await fixture(t); await f.install(); const before = f.hash();
  await f.agent("ag_preview"); await f.agent("ag_second"); const original = f.read().startups.ag_preview;
  f.revise(); f.change((db) => { db[failure] = "ag_preview"; });
  const result = await f.rollout(["ag_preview", "ag_second"], ["--apply"]);
  assert.equal(result.status, 1); assert.match(result.error, /rolled-back.*health:ag_preview/);
  assert.equal(JSON.parse((await f.call(["status"])).output).current, `sha256-${before}`);
  assert.deepEqual(f.read().startups.ag_preview, original);
  assert.equal(f.read().agents.ag_second.run_generation, 1);
  assert.equal(Object.keys(f.read().messages).length, 0);
  const replay = await f.rollout(["ag_preview", "ag_second"], ["--apply"]);
  assert.equal(replay.status, 1); assert.match(replay.error, /Previous rollout failed/);
});

test("leader/foreign machine/custom startup and incompatible bundle are rejected before restart", async (t) => {
  const f = await fixture(t); await f.install(); await f.agent("ag_preview", "leader");
  assert.match((await f.rollout(["ag_preview"], ["--apply"])).error, /leader/);
  f.change((db) => { db.agents.ag_preview.name = "preview"; db.agents.ag_preview.path = "/test/preview"; db.agents.ag_preview.server_id = "srv_foreign"; });
  assert.match((await f.rollout(["ag_preview"], ["--apply"])).error, /this Controller/);
  f.change((db) => { db.agents.ag_preview.server_id = "srv_test"; db.startups.ag_preview.command = "/owner/custom"; db.startups.ag_preview.args = []; });
  assert.match((await f.rollout(["ag_preview"], ["--apply"])).error, /Only existing launcher/);
  f.change((db) => { db.controller.capabilities = []; });
  assert.match((await f.install()).error, /lacks launcher capabilities/);
  assert.equal(f.read().calls.filter((args: string[]) => args[2] === "restart").length, 0);
});

test("a crashed operator resumes the already-accepted restart without repeating it or its continuation", async (t) => {
  const f = await fixture(t); await f.install(); await f.agent("ag_preview"); f.revise();
  f.change((db) => { db.crashRestart = true; });
  const crashed = await f.rollout(["ag_preview"], ["--apply"]); assert.notEqual(crashed.status, 0);
  const base = join(f.machine, ".treer/launchers/agent/rollouts");
  const journal = JSON.parse(readFileSync(join(base, "test.json"), "utf8"));
  assert.equal(journal.items[0].phase, "restarting");
  assert.equal(f.read().agents.ag_preview.run_generation, 2);
  rmdirSync(join(base, ".lock")); // The fixture operator exited; recover its stale lock only.
  const resumed = await f.rollout(["ag_preview"], ["--apply"]);
  assert.equal(resumed.status, 0, resumed.error); assert.equal(JSON.parse(resumed.output).status, "completed");
  assert.equal(f.read().calls.filter((args: string[]) => args[2] === "restart").length, 1);
  assert.equal(Object.keys(f.read().messages).length, 1);
});
