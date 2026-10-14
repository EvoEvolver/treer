import assert from "node:assert/strict";
import { test, type TestContext } from "node:test";
import { mkdtempSync, realpathSync, readFileSync, rmSync, appendFileSync, writeFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { pathToFileURL } from "node:url";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { ClaudeRuntime } from "../src/harness/claude.js";
import { GoalExitRequests } from "../src/goal-exit.js";
import { GoalNotifications } from "../src/goal-notify.js";
import { GoalEdits } from "../src/goal-edits.js";
import { PromptQueue } from "../src/prompt-queue.js";
import { hermeticEnv, localFetch } from "./hermetic.mjs";

const fake = resolve(import.meta.dirname, "fake-claude.mjs");
async function until(check: () => boolean, timeout = 5000) {
  const end = Date.now() + timeout;
  while (!check() && Date.now() < end) await new Promise(done => setTimeout(done, 10));
  assert.ok(check(), "condition timed out");
}
async function fixture(t: TestContext, args: string[] = [], deadline = 1000) {
  const cwd = realpathSync(mkdtempSync(join(tmpdir(), "claude-goal-control-"))), calls = join(cwd, "calls.jsonl"), configDir = join(cwd, "claude");
  const runtime = new ClaudeRuntime(process.execPath, cwd, { stateFile: join(cwd, "state"), configDir, goalCommandTimeoutMs: deadline,
    args: [fake, "--real-goal-wire", "--zero-goal-usage", "--fixture-config", configDir, "--calls-file", calls, ...args] });
  t.after(async () => { await runtime.stop(); rmSync(cwd, { recursive: true, force: true }); });
  await runtime.start();
  return { runtime, cwd, configDir, calls: () => readFileSync(calls, "utf8").trim().split("\n").map(line => JSON.parse(line)),
    idle: () => until(() => runtime.snapshot().thread?.status === "idle") };
}

test("user clear preempts the Agent's unacknowledged exit and clears its durable journal", async t => {
  const f = await fixture(t, ["--drop-first-clear"]);
  await f.runtime.updateGoal({ objective: "hold external condition" }); await f.idle();
  await f.runtime.prompt("hold");
  const journal = join(f.cwd, "exits.json"), exits = new GoalExitRequests(journal, f.runtime, () => {});
  t.after(() => exits.close());
  const receipt = exits.enqueue({ status: "done", reason: "Work complete" }); assert.equal(receipt.status, "pending");
  await until(() => f.calls().filter(call => call.message?.content?.[0]?.text === "/goal clear").length === 1);
  await f.runtime.clearGoal();
  assert.equal(f.runtime.snapshot().goal, null);
  assert.deepEqual(JSON.parse(readFileSync(journal, "utf8")), []);
  assert.ok(f.calls().some(call => call.request?.subtype === "interrupt"), "clear must not wait for an impossible Stop hook to finish");
});

test("unconfirmed exit times out, releases locks, stops retries and durably notifies manual recovery", async t => {
  const f = await fixture(t, ["--no-clear-ack"], 100);
  await f.runtime.updateGoal({ objective: "hold external condition" }); await f.idle();
  const sent: any[] = [], outbox = new GoalNotifications(join(f.cwd, "notices.json"), () => {}, async (_to, _key, body) => { sent.push(JSON.parse(body)); }, async () => ["user_fixture"]);
  f.runtime.on("goal-exit-error", goal => outbox.enqueue(goal)); t.after(() => outbox.close());
  const journal = join(f.cwd, "exits.json"), exits = new GoalExitRequests(journal, f.runtime, () => {}); t.after(() => exits.close());
  exits.enqueue({ status: "blocked", reason: "Owner action needed" });
  await until(() => f.runtime.snapshot().goal?.syncStatus === "exit_unconfirmed" && sent.length === 1);
  await until(() => JSON.parse(readFileSync(journal, "utf8")).length === 0);
  assert.match(sent[0].required_action, /manually enter \/goal clear/);
  assert.equal(f.runtime.snapshot().goal?.status, "active", "unconfirmed clear must never claim the hook was removed");
  await f.runtime.updateGoal({ tokenBudget: 200 }); // lock was released
});

test("busy objective edit steers without interruption, exposes desync and applies native goal at idle", async t => {
  const f = await fixture(t);
  await f.runtime.updateGoal({ objective: "hold old condition" }); await f.idle(); await f.runtime.prompt("hold");
  const queue = new PromptQueue(f.runtime, () => {}); t.after(() => queue.close());
  const edits = new GoalEdits(join(f.cwd, "edits.json"), f.runtime, queue, () => {}); t.after(() => edits.close());
  await edits.apply({ objective: "hold revised condition", tokenBudget: 100000, roundBudget: 20 }, "edit-one");
  await edits.apply({ objective: "hold revised condition", tokenBudget: 100000, roundBudget: 20 }, "edit-one");
  await until(() => f.runtime.snapshot().goal?.nativeObjective === "hold revised condition" && f.runtime.snapshot().goal?.syncStatus === "synced");
  const calls = f.calls();
  assert.equal(calls.filter(call => call.request?.subtype === "interrupt").length, 0);
  assert.equal(calls.filter(call => call.message?.content?.[0]?.text === "/goal hold revised condition").length, 1);
  assert.equal(calls.filter(call => call.message?.content?.[0]?.text?.startsWith("The user changed the goal.")).length, 1);
  assert.equal(f.runtime.snapshot().goal?.tokenBudget, 100000);
  assert.ok(!JSON.stringify(f.runtime.snapshot()).includes("Request interrupted by user"));
});

test("native hook's snake_case condition rejects three times then interrupt-clears and notifies", async t => {
  const f = await fixture(t);
  const notices: any[] = []; f.runtime.on("goal-exit", goal => notices.push(goal));
  await f.runtime.updateGoal({ objective: "reject-loop native shape" });
  await until(() => notices.length === 1);
  assert.equal(f.runtime.snapshot().goal, null);
  assert.equal(notices[0].rejectionCount, 3);
  assert.equal(notices[0].consecutiveRejections, 3);
  assert.match(notices[0].reason, /without new/);
  assert.equal(notices.length, 1);
  assert.ok(f.calls().some(call => call.request?.subtype === "interrupt"));
});

test("unrelated Stop hooks containing the objective do not reject, complete or mark it impossible", async t => {
  const f = await fixture(t); await f.runtime.updateGoal({ objective: "hold condition" }); await f.idle();
  const hook = { prompt_text: "Another user hook: hold condition must be mentioned in the report", result: { ok: false, impossible: true, reason: "Unrelated condition failed" } };
  for (let index = 0; index < 4; index++) (f.runtime as any).handle({ type: "system", subtype: "stop_hook_summary", uuid: `unrelated-${index}`, hook_infos: [hook], hook_errors: [], has_output: index !== 3, prevented_continuation: index !== 3 });
  assert.equal(f.runtime.snapshot().goal!.status, "active");
  assert.equal(f.runtime.snapshot().goal!.rejectionCount, 0);
  (f.runtime as any).handle({ type: "system", subtype: "stop_hook_summary", uuid: "matching-native", hook_infos: [{ prompt_text: "hold condition" }, hook], hook_errors: [], has_output: true, prevented_continuation: true });
  assert.equal(f.runtime.snapshot().goal!.status, "active", "unrelated impossible result must not poison a matching goal hook");
  assert.equal(f.runtime.snapshot().goal!.rejectionCount, 1);
  (f.runtime as any).goalEvent({ type: "attachment", attachment: { type: "goal_status", condition: "another condition", met: true } });
  assert.equal(f.runtime.snapshot().goal!.status, "active");
  (f.runtime as any).goalEvent({ type: "attachment", attachment: { type: "goal_status", condition: "hold condition", met: true } });
  assert.equal(f.runtime.snapshot().goal!.status, "done", "native met attachment shape must complete the matching goal");
});

test("session-cumulative usage baseline is excluded; edit resets epoch and reaching 100k clears", async t => {
  const f = await fixture(t, ["--session-usage-baseline", "4000000"]);
  await f.runtime.prompt("Read session baseline"); await f.idle();
  await f.runtime.updateGoal({ objective: "hold condition", tokenBudget: 100000 }); await f.idle();
  assert.equal(f.runtime.snapshot().goal?.tokensUsed, 0);
  await f.runtime.prompt("usage 50"); await f.idle(); assert.equal(f.runtime.snapshot().goal?.tokensUsed, 50);
  await f.runtime.updateGoal({ objective: "hold edited condition", tokenBudget: 100000 }); await f.idle();
  assert.equal(f.runtime.snapshot().goal?.tokensUsed, 0);
  const receipts: any[] = []; f.runtime.on("goal-exit", goal => receipts.push(goal));
  await f.runtime.prompt("usage 100001"); await until(() => receipts.length === 1);
  assert.equal(f.runtime.snapshot().goal, null);
  assert.equal(receipts[0].tokensUsed, 100001);
  assert.match(receipts[0].reason, /token budget/);
  assert.ok(receipts[0].timeUsedSeconds > 0);
});

test("JSONL old goal acknowledgement cannot overwrite a newer deferred edit", async t => {
  const f = await fixture(t);
  await f.runtime.updateGoal({ objective: "hold original" }); await f.idle(); await f.runtime.prompt("hold");
  await f.runtime.updateGoal({ objective: "hold replacement" });
  const path = join(f.configDir, "projects", f.cwd.replace(/[^a-zA-Z0-9]/g, "-"), `${f.runtime.snapshot().thread!.id}.jsonl`);
  appendFileSync(path, JSON.stringify({ type: "system", subtype: "local_command", uuid: "old-copy-envelope", content: "<local-command-stdout>Goal set: hold original</local-command-stdout>", commandRun: { command: "goal", args: "hold original" } }) + "\n");
  await (f.runtime as any).refreshGoalHistory();
  assert.equal(f.runtime.snapshot().goal?.objective, "hold replacement");
  assert.equal(f.runtime.snapshot().goal?.nativeObjective, "hold original");
  assert.equal(f.runtime.snapshot().goal?.syncStatus, "desynced");
  await f.runtime.interrupt(); await f.idle(); await f.runtime.reconcileGoal();
  await until(() => f.runtime.snapshot().goal?.syncStatus === "synced");
});

test("resume migrates an old checkpoint without session usage baseline from native history", async t => {
  const f = await fixture(t, ["--session-usage-baseline", "4000000"]);
  await f.runtime.prompt("Read session baseline"); await f.idle();
  await f.runtime.updateGoal({ objective: "hold retained condition" }); await f.idle();
  const sessionId = f.runtime.snapshot().thread!.id;
  await f.runtime.stop();
  const history = join(f.configDir, "projects", f.cwd.replace(/[^a-zA-Z0-9]/g, "-"), `${sessionId}.jsonl`);
  const beforeGoal = { type: "assistant", uuid: "prior-query", message: { id: "prior-query", model: "sonnet", content: [{ type: "text", text: "Earlier work" }], usage: { input_tokens: 4000000 } } };
  writeFileSync(history, JSON.stringify(beforeGoal) + "\n" + readFileSync(history, "utf8"));
  const checkpoint = join(f.cwd, "state.goal-usage.json"), saved = JSON.parse(readFileSync(checkpoint, "utf8"));
  delete saved.resultTokens; writeFileSync(checkpoint, JSON.stringify(saved));
  const resumed = new ClaudeRuntime(process.execPath, f.cwd, { stateFile: join(f.cwd, "state"), configDir: f.configDir,
    args: [fake, "--fixture-config", f.configDir, "--session-usage-baseline", "4000000", "--zero-goal-usage"] });
  t.after(() => resumed.stop()); await resumed.start();
  await resumed.prompt("usage 50"); await until(() => resumed.snapshot().thread?.status === "idle");
  assert.equal(resumed.snapshot().goal?.tokensUsed, 50, "resuming must not charge four million old session tokens");
});

test("HTTP user DELETE preempts an accepted Agent exit instead of returning mutation conflict", async t => {
  const cwd = realpathSync(mkdtempSync(join(tmpdir(), "claude-goal-http-"))), root = resolve(import.meta.dirname, ".."), calls = join(cwd, "calls"), portFile = join(cwd, "port");
  const provider = join(cwd, "provider.mjs");
  writeFileSync(provider, `process.argv.push(...${JSON.stringify(["--real-goal-wire", "--zero-goal-usage", "--drop-first-clear", "--clear-no-goal", "--calls-file", calls])}); await import(${JSON.stringify(pathToFileURL(fake).href)});\n`);
  const server = spawn(process.execPath, ["--import", "tsx", "src/index.ts"], { cwd: root, stdio: "pipe", env: hermeticEnv({
    HOME: cwd, AGENT_UI_CWD: cwd, AGENT_UI_PORT: "0", AGENT_UI_PORT_FILE: portFile, AGENT_UI_HARNESS: "claude", CLAUDE_BIN: provider,
    TREER_AGENT_ID: "ag_goal_http_fixture", AGENT_UI_MESSAGE_WATCH: "0", AGENT_UI_GOAL_AGENT_EXIT: "1",
  }) });
  let logs = ""; server.stderr.on("data", chunk => { logs += chunk; });
  t.after(async () => { if (server.exitCode === null) { const exit = once(server, "exit"); server.kill("SIGTERM"); await exit; } rmSync(cwd, { recursive: true, force: true }); });
  await until(() => { try { return Boolean(readFileSync(portFile, "utf8")); } catch { return false; } });
  const base = `http://127.0.0.1:${readFileSync(portFile, "utf8")}`;
  const deadline = Date.now() + 5000;
  while ((await localFetch(base + "/api/health")).status !== 200 && Date.now() < deadline) await new Promise(done => setTimeout(done, 10));
  const request = (path: string, method: string, body?: unknown) => localFetch(base + path, { method, body: body === undefined ? undefined : JSON.stringify(body) });
  let response = await request("/api/goal", "PUT", { objective: "hold external condition" }); assert.equal(response.status, 200, logs);
  response = await request("/v1/goal/exit", "POST", { status: "done", reason: "Completed except owner action" }); assert.equal(response.status, 202, logs);
  assert.equal((await response.json()).status, "pending");
  await until(() => readFileSync(calls, "utf8").includes('"text":"/goal clear"'));
  response = await request("/api/goal", "DELETE"); assert.equal(response.status, 200, logs);
  const state = await (await request("/v1/state", "GET")).json();
  assert.equal(state.goal, null);
});

test("resume preserves active budgets and an unconfirmed exit without retrying it forever", async t => {
  const f = await fixture(t, ["--no-clear-ack"], 100);
  await f.runtime.updateGoal({ objective: "hold retained goal", tokenBudget: 12345, roundBudget: 7 }); await f.idle();
  await assert.rejects(f.runtime.updateGoal({ status: "blocked", reason: "Owner action needed" }), /timed out/);
  await f.runtime.stop();
  const resumed = new ClaudeRuntime(process.execPath, f.cwd, { stateFile: join(f.cwd, "state"), configDir: f.configDir, goalCommandTimeoutMs: 100,
    args: [fake, "--fixture-config", f.configDir, "--calls-file", join(f.cwd, "calls.jsonl"), "--no-clear-ack"] });
  t.after(() => resumed.stop()); await resumed.start();
  const goal = resumed.snapshot().goal!;
  assert.equal(goal.tokenBudget, 12345); assert.equal(goal.roundBudget, 7);
  assert.equal(goal.syncStatus, "exit_unconfirmed"); assert.match(goal.syncError!, /manually enter/);
  await resumed.updateGoal({ tokenBudget: 20000 }); // no sticky pending mutation after resume
  assert.equal(resumed.snapshot().goal!.tokenBudget, 20000);
});

test("a second edit during deferred native delivery is applied without manual reconcile (Core regression)", async t => {
  const f = await fixture(t, [], 100);
  await f.runtime.prompt("hold");
  await f.runtime.updateGoal({ objective: "first edited objective" });
  let secondEdit: Promise<void> | undefined;
  f.runtime.on("state", () => {
    if (!secondEdit && (f.runtime as any).applyingDeferred && f.runtime.snapshot().thread?.status === "running") {
      secondEdit = f.runtime.updateGoal({ objective: "second edited objective" });
    }
  });
  await f.runtime.interrupt(); await until(() => Boolean(secondEdit)); await secondEdit;
  await until(() => f.runtime.snapshot().goal?.syncStatus === "synced", 400);
  assert.equal(f.runtime.snapshot().goal!.objective, "second edited objective");
  assert.equal(f.runtime.snapshot().goal!.nativeObjective, "second edited objective");
  assert.equal(f.calls().filter(call => call.message?.content?.[0]?.text === "/goal second edited objective").length, 1);
});

test("resume adopts the native objective instead of reapplying a mismatched cached edit", async t => {
  const f = await fixture(t); await f.runtime.updateGoal({ objective: "hold old native condition", tokenBudget: 100000, roundBudget: 20 }); await f.idle();
  await f.runtime.stop();
  const checkpoint = join(f.cwd, "state.goal-usage.json"), saved = JSON.parse(readFileSync(checkpoint, "utf8"));
  saved.goal.objective = "hold newer user objective"; saved.goal.tokensUsed = saved.tokensUsed = 9090000;
  delete saved.goal.syncStatus; delete saved.goal.nativeObjective; delete saved.resultTokens;
  writeFileSync(checkpoint, JSON.stringify(saved));
  const resumed = new ClaudeRuntime(process.execPath, f.cwd, { stateFile: join(f.cwd, "state"), configDir: f.configDir,
    args: [fake, "--fixture-config", f.configDir, "--zero-goal-usage"] });
  t.after(() => resumed.stop()); await resumed.start();
  await until(() => resumed.snapshot().goal?.syncStatus === "synced");
  const goal = resumed.snapshot().goal!;
  assert.equal(goal.objective, "hold old native condition"); assert.equal(goal.nativeObjective, goal.objective);
  assert.notEqual(goal.tokensUsed, 9090000);
});

test("cancelled exit cannot resurrect after user-clear timeout and late native clear (Core regression)", async t => {
  const f = await fixture(t, ["--no-clear-ack"], 150);
  await f.runtime.updateGoal({ objective: "hold external condition" }); await f.idle();
  const journal = join(f.cwd, "exits.json"), exits = new GoalExitRequests(journal, f.runtime, () => {}); t.after(() => exits.close());
  exits.enqueue({ status: "done", reason: "Cancelled Agent conclusion" });
  await until(() => f.calls().some(call => call.message?.content?.[0]?.text === "/goal clear"));
  // An idle native query must not be relied on to emit another result that
  // happens to checkpoint cancellation before a crash.
  (f.runtime as any).finish(false, "Waiting for native clear acknowledgement");
  await assert.rejects(f.runtime.clearGoal(), /timed out/);
  assert.deepEqual(JSON.parse(readFileSync(journal, "utf8")), []);
  assert.equal(JSON.parse(readFileSync(join(f.cwd, "state.goal-usage.json"), "utf8")).pendingExit, null);
  exits.close(); await f.runtime.stop();
  const path = join(f.configDir, "projects", f.cwd.replace(/[^a-zA-Z0-9]/g, "-"), `${f.runtime.snapshot().thread!.id}.jsonl`);
  appendFileSync(path, JSON.stringify({ type: "system", subtype: "local_command", uuid: "late-user-clear", content: "<local-command-stdout>Goal cleared</local-command-stdout>", commandRun: { command: "goal", args: "clear" } }) + "\n");
  const resumed = new ClaudeRuntime(process.execPath, f.cwd, { stateFile: join(f.cwd, "state"), configDir: f.configDir, args: [fake, "--fixture-config", f.configDir] });
  t.after(() => resumed.stop()); await resumed.start();
  assert.equal(resumed.snapshot().goal, null);
});

test("native No goal set acknowledges UI clear, removes the residual goal and persists null", async t => {
  const f = await fixture(t, ["--clear-no-goal"]);
  await f.runtime.updateGoal({ objective: "hold residual goal" }); await f.idle();
  await f.runtime.clearGoal();
  assert.equal(f.runtime.snapshot().goal, null);
  const saved = JSON.parse(readFileSync(join(f.cwd, "state.goal-usage.json"), "utf8"));
  assert.equal(saved.goal, null); assert.equal(saved.tokensUsed, null);
  await f.idle();
  await f.runtime.updateGoal({ objective: "hold another residual goal" }); await f.idle();
  // /goal without arguments also authoritatively reports that no native hook
  // exists; it is not limited to acknowledgements of our own clear command.
  await f.runtime.prompt("/goal"); await f.idle();
  assert.equal(f.runtime.snapshot().goal, null);
});

for (const cachedStatus of ["active", "cleared", "blocked"] as const) test(`resume ignores ${cachedStatus} cache when latest native event reports absence`, async t => {
  const f = await fixture(t); await f.runtime.updateGoal({ objective: "hold stale cache" }); await f.idle(); await f.runtime.stop();
  const checkpoint = join(f.cwd, "state.goal-usage.json"), saved = JSON.parse(readFileSync(checkpoint, "utf8"));
  saved.goal.status = cachedStatus; saved.goal.syncStatus = "desynced";
  writeFileSync(checkpoint, JSON.stringify(saved));
  const path = join(f.configDir, "projects", f.cwd.replace(/[^a-zA-Z0-9]/g, "-"), `${f.runtime.snapshot().thread!.id}.jsonl`);
  appendFileSync(path, JSON.stringify({ type: "system", subtype: "local_command", uuid: "authoritative-absence", content: "<local-command-stdout>No goal set</local-command-stdout>", commandRun: { command: "goal", args: "clear" } }) + "\n");
  const resumed = new ClaudeRuntime(process.execPath, f.cwd, { stateFile: join(f.cwd, "state"), configDir: f.configDir,
    args: [fake, "--fixture-config", f.configDir, "--calls-file", join(f.cwd, "resumed-calls")] });
  t.after(() => resumed.stop()); await resumed.start();
  assert.equal(resumed.snapshot().goal, null);
  assert.equal(JSON.parse(readFileSync(checkpoint, "utf8")).goal, null);
  assert.ok(!readFileSync(join(f.cwd, "resumed-calls"), "utf8").includes("/goal hold stale cache"));
});

test("JSONL clear is observed after completion; ordinary assistant absence claims have no authority", async t => {
  const f = await fixture(t); await f.runtime.updateGoal({ objective: "hold completed goal" }); await f.idle();
  (f.runtime as any).goalEvent({ type: "attachment", attachment: { type: "goal_status", met: true, condition: "hold completed goal" } });
  assert.equal(f.runtime.snapshot().goal?.status, "done");
  (f.runtime as any).goalEvent({ type: "assistant", message: { model: "real-model", content: [{ type: "text", text: "No goal set" }] } });
  assert.equal(f.runtime.snapshot().goal?.status, "done");
  const path = join(f.configDir, "projects", f.cwd.replace(/[^a-zA-Z0-9]/g, "-"), `${f.runtime.snapshot().thread!.id}.jsonl`);
  appendFileSync(path, JSON.stringify({ type: "system", subtype: "local_command", uuid: "external-clear", content: "<local-command-stdout>No goal set</local-command-stdout>", commandRun: { command: "goal", args: "clear" } }) + "\n");
  await until(() => f.runtime.snapshot().goal === null);
  assert.equal(JSON.parse(readFileSync(join(f.cwd, "state.goal-usage.json"), "utf8")).goal, null);
  await f.runtime.updateGoal({ objective: "hold attachment goal" }); await f.idle();
  (f.runtime as any).goalEvent({ type: "attachment", attachment: { type: "goal_status", condition: "hold attachment goal", met: true } });
  (f.runtime as any).goalEvent({ type: "attachment", attachment: { type: "goal_status", status: "cleared" } });
  assert.equal(f.runtime.snapshot().goal, null);
});
