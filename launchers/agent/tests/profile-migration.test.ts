import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync, rmSync, statSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { hermeticEnv } from "./hermetic.mjs";
import test from "node:test";
import { profileMigration } from "../scripts/profile-migration.mjs";
const definitions = JSON.parse(readFileSync(new URL("../profiles.json", import.meta.url), "utf8")).profiles;
test("dry-run converges every bundled provider across three machines, protects unrelated profiles, and never mutates input", () => {
  const profiles = definitions.flatMap((profile: any) => [
    { ...profile, command: "/Users/mac/.treer/launchers/agent/current/launchers/agent/scripts/treer-agent.sh", cwd: "treer" },
    ...["mac_m2", "mariana", "win_native"].map(machine => ({ ...profile, name: `${profile.name} (${machine})`, command: "/selected/node", cwd: ".", args: ["/machine/.treer/launchers/agent/run.mjs", ...profile.args] })),
  ]);
  profiles.push({ name: "Pockymoe Claude", command: "/owner/pockymoe", args: [], cwd: "." }, { name: "codex", command: "codex", args: ["--yolo"], cwd: "." });
  const before = structuredClone(profiles), plan = profileMigration(profiles, definitions);
  assert.equal(plan.changes.filter((change: any) => change.operation === "update").length, definitions.length);
  assert.equal(plan.changes.filter((change: any) => change.operation === "delete").length, definitions.length * 3);
  assert.deepEqual(plan.keep, ["Pockymoe Claude", "codex"]);
  assert.deepEqual(profiles, before);
  for (const change of plan.changes.filter((change: any) => change.operation === "update")) {
    assert.equal(change.next.command, "treer-agent"); assert.equal(change.next.cwd, ".");
  }
});
test("customized or referenced qualified profiles are not deleted; custom canonical profiles fail closed", () => {
  const base = definitions[0];
  const referenced = { ...base, name: base.name + " (win_native)", profile_id: "lp_ref", command: "/node", cwd: ".", args: ["/root/.treer/launchers/agent/run.mjs", ...base.args] };
  const custom = { ...base, name: base.name + " (custom)", command: "/owner/script", cwd: "." };
  const plan = profileMigration([referenced, custom], definitions, [{ launch_profile_id: "lp_ref" }]);
  assert.ok(plan.keep.includes(referenced.name)); assert.ok(plan.keep.includes(custom.name));
  assert.throws(() => profileMigration([{ ...base, command: "/owner/script", cwd: "." }], definitions), /Customized/);
});
test("a preview name and runner argument alone never authorize deleting a custom executable", () => {
  const definition = definitions[0];
  const profile = { ...definition, name: definition.name + " (React preview f7b76ae mac_m2)", command: "/owner/custom", args: ["/preview/f7b76aee3d2202a5dda92794d90a8519c0fa2ba7/scripts/treer-agent.mjs", ...definition.args] };
  assert.ok(profileMigration([profile], definitions).keep.includes(profile.name));
});
test("migration CLI dry-run is read-only, apply verifies deletion, and a failed apply restores profiles", t => {
  const root = mkdtempSync(join(tmpdir(), "profile-migration-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const state = join(root, "profiles.json"), fake = join(root, "treer.cjs");
  writeFileSync(fake, `const fs=require('node:fs'),a=process.argv.slice(2),file=process.env.PROFILE_FIXTURE;
let profiles=JSON.parse(fs.readFileSync(file));
if(a.includes('ls')){console.log(JSON.stringify({agents:[]}));process.exit()}
const [action,name]=a.slice(3);const p=profiles.find(p=>p.name===name);
if(action==='list'){console.log(JSON.stringify({profiles}));process.exit()}
if(action==='show'){if(!p){console.error(JSON.stringify({error:{code:'launch_profile_not_found'}}));process.exit(1)}console.log(JSON.stringify(p));process.exit()}
if(action==='delete'){if(process.env.FAIL_DELETE===name){console.error(JSON.stringify({error:{code:'forbidden'}}));process.exit(1)}profiles=profiles.filter(p=>p.name!==name)}
else if(action==='create'||action==='update'){const i=a.indexOf('--cwd');const next={name,description:a[a.indexOf('--description')+1],cwd:a[i+1],command:action==='create'?a[i+2]:a[a.indexOf('--command')+1],args:action==='create'?a.slice(a.indexOf('--')+1):a.flatMap((v,i)=>v==='--arg'?[a[i+1]]:[])};profiles=profiles.filter(p=>p.name!==name);profiles.push(next)}else throw Error('Unexpected call');
fs.writeFileSync(file,JSON.stringify(profiles));console.log('{}');`);
  const initial = definitions.flatMap(({ name, description, args }: any) => [name, name + " (win_native)"].map(name => ({ name, description, args, command: "/old/.treer/launchers/agent/current/launchers/agent/scripts/treer-agent.sh", cwd: "." })));
  const run = (args: string[], extra = {}) => spawnSync(process.execPath, [fileURLToPath(new URL("../scripts/migrate-profiles.mjs", import.meta.url)), ...args], { encoding: "utf8", env: hermeticEnv({ AGENT_UI_TREER_BIN: fake, PROFILE_FIXTURE: state, ...extra }) });
  // fileURLToPath handles Windows drive letters and spaces in checkout paths.
  const sorted = (value: any[]) => value.sort((a, b) => a.name.localeCompare(b.name));
  writeFileSync(state, JSON.stringify(initial));
  const dry = run(["--dry-run"]); assert.equal(dry.status, 0, dry.stderr); assert.deepEqual(JSON.parse(readFileSync(state, "utf8")), initial);
  const failed = run(["--apply"], { FAIL_DELETE: initial[1].name }); assert.notEqual(failed.status, 0);
  function verifyBackup(result: { stderr: string }) {
    const path = result.stderr.match(/^Profile backup: (.+)$/m)?.[1]?.trim();
    assert.ok(path, "apply must report the backup before mutation");
    t.after(() => rmSync(dirname(path), { recursive: true, force: true }));
    const backup = JSON.parse(readFileSync(path, "utf8"));
    assert.deepEqual(backup.profiles, initial); assert.equal(backup.schema, "treer.profile-backup/v1");
    if (process.platform !== "win32") {
      assert.equal(statSync(path).mode & 0o777, 0o600);
      assert.equal(statSync(dirname(path)).mode & 0o777, 0o700);
    }
    assert.ok(!result.stderr.includes('"command"'), "backup contents must not be logged");
  }
  verifyBackup(failed);
  assert.deepEqual(sorted(JSON.parse(readFileSync(state, "utf8"))), sorted(structuredClone(initial)));
  const applied = run(["--apply"]); assert.equal(applied.status, 0, applied.stderr);
  // The failed transaction restored the same values, with irrelevant ordering
  // differences; the successful run captures that inventory before updating.
  const path = applied.stderr.match(/^Profile backup: (.+)$/m)?.[1]?.trim(); assert.ok(path);
  t.after(() => rmSync(dirname(path), { recursive: true, force: true }));
  assert.deepEqual(sorted(JSON.parse(readFileSync(path, "utf8")).profiles), sorted(structuredClone(initial)));
  const saved = JSON.parse(readFileSync(state, "utf8")); assert.equal(saved.length, definitions.length);
  assert.ok(saved.every((p: any) => p.command === "treer-agent" && p.cwd === "."));
});
