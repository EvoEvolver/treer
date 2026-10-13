import assert from "node:assert/strict";
import { appendFileSync, mkdtempSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { CodexRuntime } from "../src/harness/codex.js";
import { ClaudeRuntime } from "../src/harness/claude.js";
import { goalUpdate, mapGoal } from "../src/goals.js";
const temp = () => realpathSync(mkdtempSync(join(tmpdir(), "agent-ui-goals-")));
const fakeCodex = join(import.meta.dirname, "fake-codex.mjs");
const fakeClaude = join(import.meta.dirname, "fake-claude.mjs");
async function until(predicate: () => boolean) {
  const deadline = Date.now() + 5000;
  while (!predicate() && Date.now() < deadline) await new Promise((done) => setTimeout(done, 10));
  assert.ok(predicate(), "Goal state timed out");
}

test("Codex native goals set, patch, pause, complete, resume and clear", async () => {
  const cwd = temp(); const stateFile = join(cwd, "state");
  const runtime = new CodexRuntime(fakeCodex, cwd, { stateFile });
  let id = "";
  try {
    await runtime.start(); id = runtime.snapshot().thread!.id;
    assert.deepEqual(runtime.harness.capabilities.goal, { pause: true, budget: true });
    await runtime.updateGoal({ objective: "Ship", tokenBudget: 1000 });
    await runtime.updateGoal({ status: "paused" });
    assert.equal(runtime.snapshot().goal!.status, "paused");
    assert.equal(runtime.snapshot().goal!.objective, "Ship");
    for (const status of ["blocked", "usageLimited", "budgetLimited"] as const) {
      await runtime.updateGoal({ status });
      assert.equal(runtime.snapshot().goal!.status, status);
    }
    await runtime.updateGoal({ objective: "Verify", status: "active", tokenBudget: null });
    await runtime.prompt("complete");
    await until(() => runtime.snapshot().goal?.status === "done");
    assert.equal(runtime.snapshot().goal!.tokensUsed, 42);
  } finally { await runtime.stop(); }
  const resumed = new CodexRuntime(fakeCodex, cwd, { stateFile, threadId: id });
  try {
    await resumed.start(); assert.equal(resumed.snapshot().goal!.objective, "Verify");
    assert.equal(resumed.snapshot().goal!.status, "done");
    await resumed.clearGoal(); assert.equal(resumed.snapshot().goal?.status, "cleared");
    await resumed.prompt("/goal New objective"); assert.equal(resumed.snapshot().goal!.objective, "New objective");
    await resumed.prompt("/goal clear"); assert.equal(resumed.snapshot().goal?.status, "cleared");
  } finally { await resumed.stop(); }
});

test("unsupported Codex goal API disables capability without blocking startup", async () => {
  process.env.FAKE_CODEX_NO_GOAL = "1";
  const runtime = new CodexRuntime(fakeCodex, temp());
  try { await runtime.start(); assert.equal(runtime.harness.capabilities.goal, null); await assert.rejects(runtime.updateGoal({ objective: "x" }), /does not support goals/); }
  finally { await runtime.stop(); delete process.env.FAKE_CODEX_NO_GOAL; }
});

test("Claude native goal acknowledgements, JSONL completion, usage checkpoint, resume and clear", async () => {
  const cwd = temp(); const stateFile = join(cwd, "state"); const configDir = join(cwd, "claude");
  process.env.FAKE_CLAUDE_CONFIG = configDir;
  const create = () => new ClaudeRuntime(process.execPath, cwd, { args: [fakeClaude], stateFile, configDir });
  const runtime = create();
  try {
    await runtime.start(); assert.deepEqual(runtime.harness.capabilities.goal, { pause: false, budget: true });
    await runtime.updateGoal({ objective: "Finish" });
    await until(() => runtime.snapshot().thread?.status === "idle");
    assert.equal(runtime.snapshot().goal!.status, "done");
    assert.equal(runtime.snapshot().goal!.tokensUsed, 17);
    assert.ok(runtime.snapshot().goal!.timeUsedSeconds! > 0);
    await assert.rejects(runtime.updateGoal({ status: "paused" }), /do not support/);
    await runtime.updateGoal({ tokenBudget: 1000 });
  } finally { await runtime.stop(); }
  const resumed = create();
  try {
    await resumed.start();
    assert.equal(resumed.snapshot().goal!.status, "done"); assert.equal(resumed.snapshot().goal!.tokensUsed, 17);
    assert.ok(resumed.snapshot().goal!.timeUsedSeconds! > 0);
    await resumed.clearGoal(); await until(() => resumed.snapshot().thread?.status === "idle"); assert.equal(resumed.snapshot().goal?.status, "cleared");
    // A stop-hook block-cap override (hasOutput true) must not fake completion.
    await resumed.prompt("/goal hold condition"); await until(() => resumed.snapshot().thread?.status === "idle");
    assert.equal(resumed.snapshot().goal!.objective, "hold condition"); assert.equal(resumed.snapshot().goal!.status, "active");
    await resumed.prompt("/goal clear"); await until(() => resumed.snapshot().thread?.status === "idle"); assert.equal(resumed.snapshot().goal?.status, "cleared");
  } finally { await resumed.stop(); delete process.env.FAKE_CLAUDE_CONFIG; }
});

test("goal inputs and status mapping reject invalid budgets and degrade unknown native status", () => {
  assert.deepEqual(goalUpdate({ objective: " hi ", tokenBudget: null }), { objective: "hi", tokenBudget: null });
  for (const body of [{}, { objective: " " }, { tokenBudget: -1 }, { tokenBudget: 1.5 }, { status: "madeUp" }]) assert.throws(() => goalUpdate(body));
  assert.equal(mapGoal({ objective: "x", status: "future" })!.status, "blocked");
});

test("Claude versions without native goal command disable the capability", async () => {
  process.env.FAKE_CLAUDE_NO_GOAL = "1";
  const runtime = new ClaudeRuntime(process.execPath, temp(), { args: [fakeClaude] });
  try {
    await runtime.start();
    assert.equal(runtime.harness.capabilities.goal, null);
    await assert.rejects(runtime.updateGoal({ objective: "x" }), /does not support goals/);
  } finally { await runtime.stop(); delete process.env.FAKE_CLAUDE_NO_GOAL; }
});

test("Claude does not replay a previous completion into a replacement goal with the same objective", async () => {
  const cwd = temp(); const configDir = join(cwd, "claude"); const stateFile = join(cwd, "state");
  process.env.FAKE_CLAUDE_CONFIG = configDir;
  const create = () => new ClaudeRuntime(process.execPath, cwd, { args: [fakeClaude], configDir, stateFile });
  const first = create(); let id = "";
  try {
    await first.start(); id = first.snapshot().thread!.id;
    await first.updateGoal({ objective: "Finish" });
    await until(() => first.snapshot().thread?.status === "idle");
  } finally { await first.stop(); }
  // A second native command after that completion, with no successful hook yet.
  const history = join(configDir, "projects", cwd.replace(/[^a-zA-Z0-9]/g, "-"), `${id}.jsonl`);
  appendFileSync(history, JSON.stringify({ type: "system", subtype: "local_command", uuid: "replacement-goal", content: "<local-command-stdout>Goal set: Finish</local-command-stdout>", commandRun: { command: "goal", args: "Finish" } }) + "\n");
  const resumed = create();
  try {
    await resumed.start(); assert.equal(resumed.snapshot().goal!.status, "active");
    await resumed.prompt("continue");
    await until(() => resumed.snapshot().thread?.status === "idle");
    assert.equal(resumed.snapshot().goal!.status, "active");
  } finally { await resumed.stop(); delete process.env.FAKE_CLAUDE_CONFIG; }
});
