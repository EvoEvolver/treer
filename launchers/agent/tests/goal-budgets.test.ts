import assert from "node:assert/strict";
import test, { type TestContext } from "node:test";
import { mkdtempSync, realpathSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { GoalGuard, goalUpdate } from "../src/goals.js";
import { goalEditPrompt } from "../src/goal-edits.js";
import { ClaudeRuntime } from "../src/harness/claude.js";
import { CodexRuntime } from "../src/harness/codex.js";
import { hermeticEnv } from "./hermetic.mjs";

test.before(() => { process.env = hermeticEnv(); });
async function until(check: () => boolean) {
  const deadline = Date.now() + 5000;
  while (!check() && Date.now() < deadline) await new Promise(done => setTimeout(done, 10));
  assert.ok(check(), "condition timed out");
}
function directory(t: TestContext) {
  const cwd = realpathSync(mkdtempSync(join(tmpdir(), "goal-budgets-")));
  t.after(() => rmSync(cwd, { recursive: true, force: true })); return cwd;
}

test("omitted budgets have no token/round ceiling, while no-progress protection stays enabled", () => {
  const guard = new GoalGuard();
  assert.equal(guard.tokenBudget, null); assert.equal(guard.roundBudget, null);
  assert.equal(guard.usage(9_000_000), null);
  for (let i = 0; i < 100; i++) { guard.progress(`tool result ${i}`); assert.equal(guard.reject(), null); }
  assert.equal(guard.reject(), null); assert.match(guard.reject()!, /without new/);
  assert.deepEqual(goalUpdate({ tokenBudget: null, roundBudget: null }), { tokenBudget: null, roundBudget: null });
  for (const roundBudget of [0, -1, 1.5, 10001]) assert.throws(() => goalUpdate({ roundBudget }));
});

test("explicit environment limits are optional and explicit null overrides them", () => {
  process.env.AGENT_UI_GOAL_ROUND_BUDGET = "2"; process.env.AGENT_UI_GOAL_TOKEN_BUDGET = "10";
  try {
    const limited = new GoalGuard(); assert.match(limited.usage(10)!, /token budget/);
    const unlimited = new GoalGuard(undefined, null, null); assert.equal(unlimited.usage(9_000_000), null);
    process.env.AGENT_UI_GOAL_ROUND_BUDGET = "0"; process.env.AGENT_UI_GOAL_TOKEN_BUDGET = "0";
    assert.equal(new GoalGuard().roundBudget, null); assert.equal(new GoalGuard().tokenBudget, null);
  } finally { delete process.env.AGENT_UI_GOAL_ROUND_BUDGET; delete process.env.AGENT_UI_GOAL_TOKEN_BUDGET; }
});

test("Claude default goals exceed the old token cap; null removes limits and persists on resume", async t => {
  const cwd = directory(t), configDir = join(cwd, "claude"), stateFile = join(cwd, "state");
  const create = () => new ClaudeRuntime(process.execPath, cwd, { stateFile, configDir,
    args: [join(import.meta.dirname, "fake-claude.mjs"), "--fixture-config", configDir, "--zero-goal-usage"] });
  const runtime = create();
  try {
    await runtime.start(); await runtime.updateGoal({ objective: "hold condition" });
    await until(() => runtime.snapshot().thread?.status === "idle");
    assert.equal(runtime.snapshot().goal!.tokenBudget, null); assert.equal(runtime.snapshot().goal!.roundBudget, null);
    await runtime.prompt("usage 150000"); await until(() => runtime.snapshot().thread?.status === "idle");
    assert.equal(runtime.snapshot().goal!.status, "active"); assert.equal(runtime.snapshot().goal!.tokensUsed, 150000);
    await runtime.updateGoal({ tokenBudget: 500000, roundBudget: 7 });
    await runtime.updateGoal({ tokenBudget: null });
    assert.equal(runtime.snapshot().goal!.tokenBudget, null); assert.equal(runtime.snapshot().goal!.roundBudget, 7);
    await runtime.updateGoal({ roundBudget: null });
  } finally { await runtime.stop(); }
  const resumed = create();
  try {
    await resumed.start(); assert.equal(resumed.snapshot().goal!.status, "active");
    assert.equal(resumed.snapshot().goal!.tokenBudget, null); assert.equal(resumed.snapshot().goal!.roundBudget, null);
  } finally { await resumed.stop(); }
});

for (const kind of ["missing", "null", "finite"]) test(`legacy Claude usage reset preserves ${kind} budgets`, async t => {
  const finite = kind === "finite";
  const cwd = directory(t), configDir = join(cwd, "claude"), stateFile = join(cwd, "state");
  const create = () => new ClaudeRuntime(process.execPath, cwd, { stateFile, configDir,
    args: [join(import.meta.dirname, "fake-claude.mjs"), "--fixture-config", configDir, "--zero-goal-usage"] });
  const runtime = create();
  try { await runtime.start(); await runtime.updateGoal({ objective: "hold legacy condition" }); await until(() => runtime.snapshot().thread?.status === "idle"); }
  finally { await runtime.stop(); }
  const file = stateFile + ".goal-usage.json", saved = JSON.parse(readFileSync(file, "utf8"));
  delete saved.resultTokens; saved.tokensUsed = saved.goal.tokensUsed = 9_000_000;
  saved.goal.tokenBudget = finite ? 100000 : null;
  if (kind === "missing") delete saved.goal.tokenBudget;
  if (finite) saved.goal.roundBudget = 20; else delete saved.goal.roundBudget;
  writeFileSync(file, JSON.stringify(saved));
  const resumed = create();
  try {
    await resumed.start(); const goal = resumed.snapshot().goal!;
    assert.equal(goal.status, "active"); assert.equal(goal.tokensUsed, 0);
    assert.equal(goal.tokenBudget, finite ? 100000 : null); assert.equal(goal.roundBudget, finite ? 20 : null);
    assert.match(goal.reason!, /Legacy per-goal usage/);
    assert.equal(JSON.parse(readFileSync(file, "utf8")).goal.tokenBudget, finite ? 100000 : null);
  } finally { await resumed.stop(); }
});

test("Codex sends null to native goals, preserves limits on status-only patch and clears them explicitly", async t => {
  const cwd = directory(t), stateFile = join(cwd, "state"), calls = join(cwd, "calls.jsonl");
  process.env.FAKE_CODEX_CALLS = calls;
  const create = () => new CodexRuntime(join(import.meta.dirname, "fake-codex.mjs"), cwd, { stateFile });
  const runtime = create();
  try {
    await runtime.start(); await runtime.updateGoal({ objective: "Unlimited objective" });
    assert.equal(runtime.snapshot().goal!.tokenBudget, null); assert.equal(runtime.snapshot().goal!.roundBudget, null);
    const native = readFileSync(calls, "utf8").trim().split("\n").map(line => JSON.parse(line)).find(call => call.method === "thread/goal/set");
    assert.equal(native.params.tokenBudget, null);
    for (let index = 0; index < 25; index++) await (runtime as any).onNotification({ method: "turn/completed", params: { threadId: runtime.snapshot().thread!.id, turn: { id: `unlimited-${index}`, status: "completed", items: [] } } });
    assert.equal(runtime.snapshot().goal!.status, "active"); assert.equal(runtime.snapshot().goal!.roundsUsed, 25);
    await runtime.updateGoal({ tokenBudget: 100000, roundBudget: 20 });
    await runtime.updateGoal({ status: "paused" });
    assert.equal(runtime.snapshot().goal!.tokenBudget, 100000); assert.equal(runtime.snapshot().goal!.roundBudget, 20);
    await runtime.updateGoal({ tokenBudget: null, roundBudget: null });
  } finally { await runtime.stop(); delete process.env.FAKE_CODEX_CALLS; }
  const resumed = create();
  try { await resumed.start(); assert.equal(resumed.snapshot().goal!.tokenBudget, null); assert.equal(resumed.snapshot().goal!.roundBudget, null); }
  finally { await resumed.stop(); }
});

for (const finite of [false, true]) test(`Codex resume preserves ${finite ? "legacy numeric budgets" : "legacy missing/null unlimited budgets"}`, async t => {
  const cwd = directory(t), stateFile = join(cwd, "state");
  const create = () => new CodexRuntime(join(import.meta.dirname, "fake-codex.mjs"), cwd, { stateFile });
  const runtime = create();
  try { await runtime.start(); await runtime.updateGoal({ objective: "Legacy native goal", tokenBudget: finite ? 100000 : null, roundBudget: finite ? 20 : null }); }
  finally { await runtime.stop(); }
  const savedFile = stateFile + ".goal-exit.json", saved = JSON.parse(readFileSync(savedFile, "utf8"));
  if (!finite) { delete saved.guard.roundBudget; writeFileSync(savedFile, JSON.stringify(saved)); }
  const resumed = create();
  try {
    await resumed.start(); assert.equal(resumed.snapshot().goal!.status, "active");
    assert.equal(resumed.snapshot().goal!.tokenBudget, finite ? 100000 : null);
    assert.equal(resumed.snapshot().goal!.roundBudget, finite ? 20 : null);
  } finally { await resumed.stop(); }
});

test("goal edit context explicitly tells the model unlimited rather than default", () => {
  const prompt = goalEditPrompt(null, { objective: "Work", status: "active", tokenBudget: null, tokensUsed: 0, timeUsedSeconds: 0 });
  assert.match(prompt, /Token budget: unlimited\. Round budget: unlimited\./); assert.ok(!prompt.includes("default"));
});
