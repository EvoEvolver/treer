import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { profileFailureSummary } from "../scripts/profile-transaction.mjs";
import { hermeticEnv } from "./hermetic.mjs";
import { execFileSync } from "node:child_process";

test("profile diagnostics expose only bounded known codes and never arbitrary output", () => {
  const secret = "token=synthetic-secret C:\\private\\root\n\r\u202e" + "x".repeat(400);
  for (const code of ["cli_failed", "launch_profile_exists", "launch_profile_not_found"]) {
    const message = profileFailureSummary({ status: 1, stderr: JSON.stringify({ error: { code, message: secret } }), stdout: secret });
    assert.match(message, new RegExp(code)); assert.ok(message.length <= 300);
    assert.ok(!message.includes("synthetic")); assert.ok(!/[\r\n\u202e]/.test(message));
  }
  for (const value of [secret, JSON.stringify({ error: { code: secret, message: secret } })]) {
    assert.equal(profileFailureSummary({ status: 1, stderr: value }), "Treer profile operation failed (exit 1; details withheld)");
  }
  assert.match(profileFailureSummary({ error: { code: "ETIMEDOUT", message: secret } }), /ETIMEDOUT/);
  assert.ok(!profileFailureSummary({ error: { code: secret, message: secret } }).includes("synthetic"));
});

test("show/create/update/restore preserve safe failure codes and operator discovery binds machine identity", () => {
  const root = mkdtempSync(join(tmpdir(), "profile-diagnostics-"));
  try {
    const cli = join(root, "cli.mjs"), driver = join(root, "driver.mjs");
    writeFileSync(cli, `if(process.argv.slice(2).join(' ')==='machine list')console.log(JSON.stringify([{server_id:'srv_other',name:'other'},{server_id:'srv_test',name:'native'}]));else{console.error(JSON.stringify({error:{code:'cli_failed',message:'synthetic token=secret /private/root'}}));process.exitCode=1;}`);
    writeFileSync(driver, `import assert from 'node:assert/strict';import {profileTransaction} from ${JSON.stringify(new URL("../scripts/profile-transaction.mjs", import.meta.url).href)};
const tx=profileTransaction(${JSON.stringify(cli)},()=>{});
assert.equal(tx.machine('srv_test').name,'native');assert.throws(()=>tx.machine('srv_missing'),/verify launcher machine name/);
for(const call of [()=>tx.show('name'),()=>tx.saveProfile({name:'name',description:'description',args:[]},'node','.',null),()=>tx.saveProfile({name:'name',description:'description',args:[]},'node','.',{}),()=>tx.restoreProfile({name:'name',previous:null})]){assert.throws(call,e=>/cli_failed/.test(e.message)&&!e.message.includes('synthetic'));}`);
    execFileSync(process.execPath, [driver], { env: hermeticEnv(), stdio: "pipe" });
  } finally { rmSync(root, { recursive: true, force: true }); }
});
