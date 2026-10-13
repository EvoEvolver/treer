import assert from "node:assert/strict";
import { test } from "node:test";
import { EventEmitter, once } from "node:events";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import { mkdtempSync, realpathSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { GoalGuard } from "../src/goals.js";
import { ClaudeRuntime } from "../src/harness/claude.js";
import { CodexRuntime } from "../src/harness/codex.js";
import { GoalNotifications } from "../src/goal-notify.js";
import { GoalExitRequests } from "../src/goal-exit.js";
import { hermeticEnv, localFetch } from "./hermetic.mjs";
const root = resolve(import.meta.dirname, "..");
const temp = () => realpathSync(mkdtempSync(join(tmpdir(), "goal-guards-")));
async function until(test: () => boolean) {
  const deadline = Date.now() + 5000;
  while (!test() && Date.now() < deadline) await new Promise((done) => setTimeout(done, 10));
  assert.ok(test(), "condition timed out");
}

test("no-progress detection counts distinct evidence, not repeated tools or prose", () => {
  const guard = new GoalGuard(3, 20, 10000);
  assert.equal(guard.reject(), null); assert.equal(guard.reject(), null);
  guard.progress("Read:fileA:result1"); assert.equal(guard.reject(), null);
  guard.progress("Read:fileA:result1"); assert.equal(guard.reject(), null);
  assert.match(guard.reject()!, /without new/);
  assert.equal(guard.totalRejections, 5);
});

for (const [objective, env, reason] of [
  ["reject-loop", {}, /without new/],
  ["progress-loop", { AGENT_UI_GOAL_ROUND_BUDGET: "2" }, /round budget/],
  ["token-loop", { AGENT_UI_GOAL_TOKEN_BUDGET: "100" }, /token budget/],
  ["impossible-loop", { FAKE_CLAUDE_OLD_GOAL: "1" }, /Owner must create environment/],
] as const) {
  test(`Claude ${objective} clears the real fake native hook and persists blocked state`, async () => {
    const cwd = temp(), stateFile = join(cwd, "state");
    const previous = Object.fromEntries(Object.keys(env).map((key) => [key, process.env[key]]));
    Object.assign(process.env, env);
    const runtime = new ClaudeRuntime(join(root, "tests/fake-claude.mjs"), cwd, { stateFile });
    let exits = 0; runtime.on("goal-exit", () => { exits++; });
    try {
      await runtime.start(); await runtime.updateGoal({ objective });
      await until(() => runtime.snapshot().goal?.status === "blocked" && exits === 1);
      assert.match(runtime.snapshot().goal!.reason!, reason);
      assert.equal(exits, 1);
      await until(() => runtime.snapshot().thread?.status === "idle");
      assert.ok(runtime.snapshot().thread?.turns.some((turn) => turn.items.some((item) => item.text.startsWith("Goal cleared"))));
      assert.equal(JSON.parse(readFileSync(stateFile + ".goal-usage.json", "utf8")).goal.status, "blocked");
    } finally { await runtime.stop(); for (const [key, value] of Object.entries(previous)) { if (value === undefined) delete process.env[key]; else process.env[key] = value; } }
  });
}

test("Claude explicit exits cite user closure, persist across resume, and never parse closure prose automatically", async () => {
  const cwd = temp(), configDir = join(cwd, "claude"), stateFile = join(cwd, "state");
  process.env.FAKE_CLAUDE_CONFIG = configDir;
  const create = () => new ClaudeRuntime(join(root, "tests/fake-claude.mjs"), cwd, { stateFile, configDir });
  const runtime = create();
  try {
    await runtime.start(); await runtime.updateGoal({ objective: "hold external condition" });
    await until(() => runtime.snapshot().thread?.status === "idle");
    await runtime.prompt("按现状结案"); await until(() => runtime.snapshot().thread?.status === "idle");
    assert.equal(runtime.snapshot().goal?.status, "active");
    await runtime.updateGoal({ status: "done", reason: 'User said "按现状结案"' });
    assert.equal(runtime.snapshot().goal?.status, "done");
  } finally { await runtime.stop(); }
  const resumed = create();
  try { await resumed.start(); assert.equal(resumed.snapshot().goal?.status, "done"); assert.match(resumed.snapshot().goal!.reason!, /按现状结案/); }
  finally { await resumed.stop(); delete process.env.FAKE_CLAUDE_CONFIG; }
});

test("Codex blocked/stale exits clear native continuation and restore launcher outcome", async () => {
  const cwd = temp(), stateFile = join(cwd, "state");
  const runtime = new CodexRuntime(join(root, "tests/fake-codex.mjs"), cwd, { stateFile });
  try {
    await runtime.start(); await runtime.updateGoal({ objective: "Deploy X" });
    await runtime.updateGoal({ status: "stale", reason: "Project removed", requiredAction: "Owner: pick a new goal" });
    assert.equal(JSON.parse(readFileSync(join(cwd, ".fake-codex-goal.json"), "utf8")), null);
  } finally { await runtime.stop(); }
  const resumed = new CodexRuntime(join(root, "tests/fake-codex.mjs"), cwd, { stateFile });
  try { await resumed.start(); assert.equal(resumed.snapshot().goal?.status, "stale"); }
  finally { await resumed.stop(); }
});

for (const harness of ["codex", "claude"] as const) for (const replacement of [false, true]) test(`${harness} crash after native clear preserves ${replacement ? "a replacement goal" : "the terminal exit"}`, async () => {
  const cwd = temp(), stateFile = join(cwd, "state"), requestFile = join(cwd, "exits.json"), configDir = join(cwd, "claude");
  const oldConfig = process.env.FAKE_CLAUDE_CONFIG; process.env.FAKE_CLAUDE_CONFIG = configDir;
  const create = () => harness === "codex" ? new CodexRuntime(join(root, "tests/fake-codex.mjs"), cwd, { stateFile }) : new ClaudeRuntime(join(root, "tests/fake-claude.mjs"), cwd, { stateFile, configDir });
  const runtime = create();
  const input = { status: "blocked" as const, reason: "External owner required", requiredAction: "Owner: provision environment" };
  try {
    await runtime.start(); await runtime.updateGoal({ objective: "hold external condition" });
    await until(() => runtime.snapshot().thread?.status === "idle");
    const pending = new GoalExitRequests(requestFile, runtime, () => {}); pending.enqueue(input); pending.close();
    // Inject a crash immediately after native release, before updateGoal can
    // store its terminal receipt. The durable intent must already exist.
    if (harness === "codex") {
      const client = (runtime as any).client, request = client.request.bind(client);
      client.request = async (method: string, params: unknown) => { const result = await request(method, params); if (method === "thread/goal/clear") throw new Error("crash after native clear"); return result; };
    } else {
      const adapter = runtime as any, command = adapter.goalCommand.bind(adapter);
      adapter.goalCommand = async (args: string) => { await command(args); if (args === "clear") throw new Error("crash after native clear"); };
    }
    await assert.rejects(runtime.updateGoal(input), /crash after native clear/);
    if (replacement) await runtime.updateGoal({ objective: "hold replacement goal" });
    await runtime.stop();
    const resumed = create(); await resumed.start();
    const requests = new GoalExitRequests(requestFile, resumed, () => {});
    try {
      await until(() => JSON.parse(readFileSync(requestFile, "utf8")).length === 0);
      if (replacement) {
        assert.equal(resumed.snapshot().goal?.status, "active"); assert.equal(resumed.snapshot().goal?.objective, "hold replacement goal");
      } else {
        assert.equal(resumed.snapshot().goal?.status, "blocked");
        assert.equal(resumed.snapshot().goal?.reason, input.reason);
        assert.equal(resumed.snapshot().goal?.requiredAction, input.requiredAction);
        let notices = 0;
        const notifications = new GoalNotifications(join(cwd, "notices.json"), () => {}, async (_to, _key, body) => {
          assert.equal(JSON.parse(body).required_action, input.requiredAction); notices++;
        }, async () => ["user_fixture"]);
        notifications.enqueue(resumed.snapshot().goal!); await until(() => notices === 1); notifications.close();
      }
    } finally { requests.close(); await resumed.stop(); }
  } finally { await runtime.stop(); if (oldConfig === undefined) delete process.env.FAKE_CLAUDE_CONFIG; else process.env.FAKE_CLAUDE_CONFIG = oldConfig; }
});

for (const harness of ["codex", "claude"] as const) test(`${harness} refuses overlapping goal edits while an exit is pending`, async () => {
  const cwd = temp();
  const runtime = harness === "codex" ? new CodexRuntime(join(root, "tests/fake-codex.mjs"), cwd) : new ClaudeRuntime(join(root, "tests/fake-claude.mjs"), cwd);
  try {
    await runtime.start(); await runtime.updateGoal({ objective: "hold external condition" });
    const exit = runtime.updateGoal({ status: "blocked", reason: "No external environment" });
    await assert.rejects(runtime.updateGoal({ objective: "Replacement" }), /already pending/);
    await assert.rejects(runtime.clearGoal(), /already pending/);
    await exit; assert.equal(runtime.snapshot().goal?.status, "blocked");
  } finally { await runtime.stop(); }
});

test("Claude budget edits still release the goal after the mutation lock completes", async () => {
  // The ordinary fixture previously reported no usage. Relying on incidental
  // /goal command usage made this assertion depend on native-ack scheduling.
  const runtime = new ClaudeRuntime(process.execPath, temp(), { args: [join(root, "tests/fake-claude.mjs"), "--zero-goal-usage"] });
  try {
    await runtime.start(); await runtime.updateGoal({ objective: "hold external condition" });
    await until(() => runtime.snapshot().thread?.status === "idle");
    await runtime.prompt("Report current state with usage");
    const turnId = runtime.snapshot().thread!.activeTurnId;
    await until(() => runtime.snapshot().thread?.turns.some((turn) => turn.id === turnId && turn.status === "completed") === true && (runtime.snapshot().goal?.tokensUsed ?? 0) === 21);
    assert.equal(runtime.snapshot().goal?.tokensUsed, 21);
    await runtime.updateGoal({ tokenBudget: 1 });
    await until(() => runtime.snapshot().goal?.status === "blocked");
    assert.match(runtime.snapshot().goal!.reason!, /token budget/);
  } finally { await runtime.stop(); }
});

test("goal notifications survive failure/restart and use identical Treer message idempotency keys", async () => {
  const file = join(temp(), "outbox.json"), keys: string[] = [];
  let failures = 0;
  const first = new GoalNotifications(file, () => { failures++; }, async (_to, key) => { keys.push(key); throw new Error("offline"); }, async () => ["user_fixture"], 10000);
  first.enqueue({ objective: "Deploy", status: "blocked", reason: "External owner needed", requiredAction: "Owner: enable environment", tokenBudget: 1000, tokensUsed: 10, timeUsedSeconds: 1 });
  await until(() => failures === 1); first.close();
  let sent = false;
  const restored = new GoalNotifications(file, () => {}, async (to, key, body) => {
    keys.push(key); assert.equal(to, "user_fixture"); assert.equal(JSON.parse(body).status, "blocked");
    assert.equal(JSON.parse(body).required_action, "Owner: enable environment"); sent = true;
  });
  await until(() => sent); assert.equal(keys[0], keys[1]); restored.close();
  const again = new GoalNotifications(file, () => {}, async () => { throw new Error("Duplicate notification must not be resent"); });
  again.enqueue({ objective: "Deploy", status: "blocked", reason: "External owner needed", requiredAction: "Owner: enable environment", tokenBudget: 1000, tokensUsed: 10, timeUsedSeconds: 1 });
  assert.equal(JSON.parse(readFileSync(file, "utf8")).pending.length, 0); again.close();
});

for (const disabled of [false, true]) test(`Agent goal command ${disabled ? "respects the disable switch" : "releases Claude hook, exposes reason in AIS state and sends user notification"}`, async () => {
  const cwd = temp(), portFile = join(cwd, "port"), log = join(cwd, "sent.json");
  writeFileSync(join(cwd, "treer"), `#!/usr/bin/env node\nimport('node:fs').then(({writeFileSync})=>{let text='';process.stdin.on('data',c=>text+=c);process.stdin.on('end',()=>{writeFileSync(${JSON.stringify(log)},text);console.log('{}')})});\n`, { mode: 0o700 });
  const child = spawn(process.execPath, ["--import", "tsx", "src/index.ts"], { cwd: root, env: hermeticEnv({
    HOME: cwd, PATH: `${cwd}:${process.env.PATH}`, AGENT_UI_CWD: cwd, AGENT_UI_PORT: "0", AGENT_UI_PORT_FILE: portFile,
    AGENT_UI_HARNESS: "claude", CLAUDE_BIN: join(root, "tests/fake-claude.mjs"), TREER_AGENT_ID: "ag_goal_fixture",
    AGENT_UI_MESSAGE_WATCH: "0", AGENT_UI_GOAL_NOTIFY_TO: "user_fixture",
    AGENT_UI_GOAL_AGENT_EXIT: disabled ? "0" : "1",
  }), stdio: "pipe" });
  let output = ""; child.stderr.on("data", (chunk) => { output += chunk; });
  try {
    await until(() => { try { return Boolean(readFileSync(portFile, "utf8")); } catch { return false; } });
    const base = `http://127.0.0.1:${readFileSync(portFile, "utf8")}`;
    const readyDeadline = Date.now() + 5000;
    while ((await localFetch(base + "/api/health")).status !== 200 && Date.now() < readyDeadline) await new Promise((done) => setTimeout(done, 10));
    const set = await localFetch(base + "/api/goal", { method: "PUT", body: JSON.stringify({ objective: "hold external condition" }) });
    assert.equal(set.status, 200, output);
    const exit = promisify(execFile)(process.execPath, [join(root, "scripts/goal.mjs"), "blocked", "--reason", "No deployment environment", "--required-action", "Owner: create environment"], { env: hermeticEnv({ AGENT_UI_URL: base }) });
    if (disabled) {
      await assert.rejects(exit, /disabled/);
      const state = await (await localFetch(base + "/v1/state")).json(); assert.equal(state.goal.status, "active"); return;
    }
    await exit;
    await until(() => { try { return JSON.parse(readFileSync(log, "utf8")).status === "blocked"; } catch { return false; } });
    const state = await (await localFetch(base + "/v1/state")).json();
    assert.equal(state.goal.status, "blocked"); assert.equal(state.goal.requiredAction, "Owner: create environment");
  } finally { if (child.exitCode === null) { const exited = once(child, "exit"); child.kill("SIGTERM"); await exited; } }
});
