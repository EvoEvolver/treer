// Self-test of console-tasks-acceptance.mjs against the local stand-in with a REAL launcher running the Claude harness
// (fake Claude, tests/fake-claude.mjs). Proves the script's mechanics: it passes a launcher with the #184 retention and
// fails when its expected bound is wrong. It does not model Treer's Proxy/Console handoff (see README).
// Usage: node tests/acceptance/tasks-self-test.mjs   (Chromium and Firefox; SELF_TEST_PW_NODE_MODULES as in self-test.mjs)
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, symlink, copyFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { app, agentId, workspacePath, cleanup, startLauncher, startStandIn, writeState } from "./standin.mjs";

const script = join(import.meta.dirname, "console-tasks-acceptance.mjs");
async function scriptPath() {
  if (!process.env.SELF_TEST_PW_NODE_MODULES) return script;
  const directory = await mkdtemp(join(tmpdir(), "tasks-script-")); cleanup.push(() => rm(directory, { recursive: true, force: true }));
  await symlink(process.env.SELF_TEST_PW_NODE_MODULES, join(directory, "node_modules"));
  const copy = join(directory, "console-tasks-acceptance.mjs"); await copyFile(script, copy);
  return copy;
}
function execute(file, env) {
  return new Promise((done) => {
    const child = spawn(process.execPath, [file], { cwd: app, env, stdio: "pipe" });
    let stdout = "", stderr = "";
    const timer = setTimeout(() => child.kill("SIGKILL"), 900_000);
    child.stdout.on("data", (c) => { stdout += c; }); child.stderr.on("data", (c) => { stderr += c; });
    child.on("exit", (status) => { clearTimeout(timer); done({ status, stdout, stderr }); });
  });
}
async function run(path, consolePort, statePath, extra = {}) {
  const reportFile = join(tmpdir(), `tasks-report-${randomUUID()}.json`);
  const result = await execute(path, { ...process.env, ACCEPT_CONSOLE_URL: `http://127.0.0.1:${consolePort}`, ACCEPT_WORKSPACE_PATH: workspacePath, ACCEPT_AGENT_ID: agentId, ACCEPT_STORAGE_STATE: statePath, ACCEPT_ENGINES: process.env.SELF_TEST_ENGINES || "chromium,firefox", ACCEPT_ALLOWED_HOST_SUFFIXES: "localhost,127.0.0.1", ACCEPT_ALLOW_HTTP: "1", ACCEPT_TIMEOUT_MS: "10000", ACCEPT_REPORT_FILE: reportFile, ...extra });
  return { ...result, reportFile };
}

try {
  const runPath = await scriptPath();
  const launcherPort = await startLauncher("claude");
  const stateDir = await mkdtemp(join(tmpdir(), "tasks-state-")); cleanup.push(() => rm(stateDir, { recursive: true, force: true }));
  const statePath = join(stateDir, "state.json"); await writeState(statePath);
  // good: retention holds (20 finished + 1 running), the clear flow works end to end, no console errors.
  const good = await startStandIn(launcherPort, "good");
  const ok = await run(runPath, good.consolePort, statePath);
  assert.equal(ok.status, 0, `good: exit ${ok.status}\n${ok.stdout}${ok.stderr}`.slice(0, 5000));
  const report = JSON.parse(await readFile(ok.reportFile, "utf8"));
  assert.equal(report.passed, true);
  for (const engine of report.results) {
    assert.equal(engine.observed.records_before, 21); assert.equal(engine.observed.running_before, 1); assert.equal(engine.observed.records_after, 1);
    assert.equal(engine.steps.filter((entry) => entry.status !== "pass").length, 0, engine.engine);
  }
  assert.ok(!/sess=ok|"value":"ok"|set-cookie/i.test(JSON.stringify(report)), "no cookie data in the report");
  console.log("ok good: 21 records (1 running + 20 finished), Keep/Escape/Clear keyboard flow, 1 record after, report clean");
  // a build that kept a different number of finished tasks would not match the expected bound: the script must fail on it.
  const wrong = await startStandIn(launcherPort, "good");
  const failed = await run(runPath, wrong.consolePort, statePath, { ACCEPT_EXPECT_FINISHED: "19", ACCEPT_TIMEOUT_MS: "6000" });
  assert.equal(failed.status, 1, `wrong bound: exit ${failed.status}\n${failed.stdout}${failed.stderr}`.slice(0, 3000));
  assert.ok(/retention/.test(failed.stdout) && /FAIL/.test(failed.stdout), "the retention step is the one that fails");
  console.log("ok wrong expected bound (19): fails at the retention step");
  // the script refuses to start without its scope or inputs (exit 2, no browser).
  for (const [label, extra] of [["no allow-list", { ACCEPT_ALLOWED_HOST_SUFFIXES: "" }], ["bare domain", { ACCEPT_ALLOWED_HOST_SUFFIXES: "example.com" }]]) {
    const refused = await run(runPath, good.consolePort, statePath, extra);
    assert.equal(refused.status, 2, label);
  }
  const missing = await execute(script, { PATH: process.env.PATH });
  assert.equal(missing.status, 2);
  console.log("ok scope guards and missing inputs refused (exit 2)");
} finally {
  for (const task of cleanup.reverse()) await task().catch(() => {});
}
