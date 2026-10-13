import assert from "node:assert/strict";
import { test } from "node:test";
import { EventEmitter } from "node:events";
import { mkdtempSync, realpathSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Goal, GoalUpdate } from "../src/model.js";
import type { HarnessRuntime } from "../src/runtime.js";
import { GoalEdits } from "../src/goal-edits.js";
import { PromptQueue } from "../src/prompt-queue.js";
import { CodexRuntime } from "../src/harness/codex.js";
const temp = () => realpathSync(mkdtempSync(join(tmpdir(), "goal-edits-")));
class Fake extends EventEmitter {
  harness = { capabilities: { steer: true } };
  goal: Goal | null = { objective: "Old", status: "active", tokenBudget: 1000, tokensUsed: 0, timeUsedSeconds: 0 };
  status = "running";
  prompts: string[] = [];
  updates = 0;
  failPrompt = false;
  snapshot() { return { ready: true, goal: this.goal, thread: { status: this.status, activeTurnId: "turn-current", turns: [] } }; }
  async updateGoal(input: GoalUpdate) { this.updates++; this.goal = { ...this.goal!, ...input }; this.emit("state"); }
  async prompt(text: string) { if (this.failPrompt) throw new Error("temporary prompt error"); this.prompts.push(text); this.status = "running"; }
}
const runtime = (fake: Fake) => fake as unknown as HarnessRuntime;
test("busy goal edits steer full English objective once and notify pause/resume/budget changes", async () => {
  const fake = new Fake(), queue = new PromptQueue(runtime(fake), () => {}), edits = new GoalEdits(join(temp(), "edits.json"), runtime(fake), queue, () => {});
  try {
    await edits.apply({ objective: "New full objective\nDeploy X" }, "edit-one");
    await edits.apply({ objective: "New full objective\nDeploy X" }, "edit-one");
    assert.equal(fake.updates, 1); assert.equal(fake.prompts.length, 1);
    assert.match(fake.prompts[0], /New full objective\nDeploy X/); assert.match(fake.prompts[0], /Previous objective: Old/);
    await edits.apply({ status: "paused" }, "pause"); assert.match(fake.prompts.at(-1)!, /Pause goal-directed/);
    await edits.apply({ status: "active", tokenBudget: 2000 }, "resume"); assert.match(fake.prompts.at(-1)!, /Token budget: 2000/);
    assert.equal(edits.state?.status, "sent");
  } finally { edits.close(); queue.close(); }
});
test("idle edit triggers a queued prompt; unsupported steering stays queued without interrupt", async () => {
  for (const busy of [false, true]) {
    const fake = new Fake(); fake.status = busy ? "running" : "idle"; fake.harness.capabilities.steer = false;
    const queue = new PromptQueue(runtime(fake), () => {}), edits = new GoalEdits(undefined, runtime(fake), queue, () => {});
    try {
      await edits.apply({ objective: "Updated" }, "update");
      if (busy) { assert.equal(fake.prompts.length, 0); assert.equal(queue.entries.length, 1); }
      else assert.equal(fake.prompts.length, 1);
    } finally { edits.close(); queue.close(); }
  }
});
test("failed delivery retries without repeating the native edit and preserves state after restart", async () => {
  const file = join(temp(), "edits.json"), fake = new Fake(); fake.failPrompt = true;
  const queue = new PromptQueue(runtime(fake), () => {}), edits = new GoalEdits(file, runtime(fake), queue, () => {});
  await edits.apply({ objective: "Retry me" }, "edit");
  assert.equal(edits.state?.status, "failed"); assert.equal(fake.updates, 1); edits.close();
  fake.failPrompt = false;
  const restored = new GoalEdits(file, runtime(fake), queue, () => {});
  await restored.apply({ objective: "Retry me" }, "edit");
  assert.equal(fake.updates, 1); assert.equal(fake.prompts.length, 1); assert.equal(restored.state?.status, "sent");
  restored.close(); queue.close();
});
test("fake Codex goal/set alone emits no prompt; launcher sends idle turn/start then busy steer", async () => {
  const cwd = temp(), calls = join(cwd, "calls"); process.env.FAKE_CODEX_CALLS = calls;
  const runtime = new CodexRuntime(join(import.meta.dirname, "fake-codex.mjs"), cwd);
  let queue: PromptQueue | undefined, edits: GoalEdits | undefined;
  try {
    await runtime.start(); await runtime.updateGoal({ objective: "Old" });
    const read = () => readFileSync(calls, "utf8").trim().split("\n").map((line) => JSON.parse(line));
    assert.equal(read().filter((call) => ["turn/start", "turn/steer"].includes(call.method)).length, 0);
    queue = new PromptQueue(runtime, () => {}); edits = new GoalEdits(undefined, runtime, queue, () => {});
    await edits.apply({ objective: "New" }, "idle");
    assert.ok(read().some((call) => call.method === "turn/start" && call.params.input[0].text.includes("New objective")));
    Object.assign(runtime.snapshot().thread!, { status: "running", activeTurnId: "turn-live" });
    await edits.apply({ objective: "Newest" }, "busy");
    assert.ok(read().some((call) => call.method === "turn/steer" && call.params.input[0].text.includes("Newest")));
  } finally { edits?.close(); queue?.close(); await runtime.stop(); delete process.env.FAKE_CODEX_CALLS; }
});
