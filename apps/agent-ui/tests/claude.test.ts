import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ClaudeRuntime, mapClaudeBlock } from "../src/harness/claude.js";
import { loadThreadId } from "../src/thread-state.js";
import { PromptQueue } from "../src/prompt-queue.js";
import { taskOutput } from "../src/task-output.js";

const fake = fileURLToPath(new URL("./fake-claude.mjs", import.meta.url));
const temp = () => realpathSync(mkdtempSync(join(tmpdir(), "agent-ui-claude-")));
function runtime(cwd: string, options: ConstructorParameters<typeof ClaudeRuntime>[2] = {}) {
  return new ClaudeRuntime(process.execPath, cwd, { args: [fake], ...options });
}
async function until(predicate: () => boolean) {
  const deadline = Date.now() + 5000;
  while (!predicate() && Date.now() < deadline) await new Promise((done) => setTimeout(done, 10));
  assert.ok(predicate(), "Timed out waiting for Claude state");
}

test("native Claude starts, persists identity, streams shared items and maintains background lifecycle", async () => {
  const cwd = temp();
  const stateFile = join(cwd, "state.json");
  const claude = runtime(cwd, { stateFile });
  const states: string[] = [];
  const partials: string[] = [];
  claude.on("state", () => {
    const state = claude.snapshot();
    if (state.backgroundTasks.length) states.push(state.backgroundTasks[0].status);
    partials.push(state.thread?.turns.at(-1)?.items.find((item) => item.kind === "reasoning")?.text ?? "");
  });
  try {
    await claude.start();
    assert.equal(loadThreadId(stateFile), undefined); // No native session exists before the first prompt.
    assert.deepEqual(claude.harness.capabilities, { steer: true, backgroundTasks: true, goal: null });
    await claude.prompt("work");
    await until(() => claude.snapshot().thread?.status === "idle");
    assert.equal(loadThreadId(stateFile), claude.snapshot().thread?.id);
    const snapshot = claude.snapshot();
    const items = snapshot.thread!.turns.at(-1)!.items;
    assert.deepEqual(items.map((item) => item.kind), ["userMessage", "reasoning", "commandExecution", "fileChange", "fileChange", "fileChange", "plan", "toolCall", "agentMessage"]);
    assert.equal(items[1].text, "Considering");
    assert.ok(partials.includes("Considering"));
    assert.equal(items[2].status, "completed");
    assert.equal(items[2].detailText, "ok");
    assert.match(items[3].changes![0].diff, /@@ -1,1 \+1,1 @@\n-old\n\+new/);
    assert.match(items[4].changes![0].diff, /--- \/dev\/null/);
    assert.equal(items[5].changes!.length, 2);
    assert.deepEqual(items[6].steps, [{ step: "Inspect", status: "completed" }, { step: "Verify", status: "inProgress" }]);
    assert.equal(items.filter((item) => item.text === "Done").length, 1);
    assert.ok(states.includes("running") && states.includes("completed"));
    const task = snapshot.backgroundTasks.find((task) => task.id === "task1")!;
    assert.equal(task.kind, "shell"); assert.equal(task.exitCode, 0); assert.equal(task.summary, "TASK_DONE"); assert.ok(task.endedAt);
    assert.equal((await taskOutput(task, claude.taskOutputRoots, "10")).content, "TASK_DONE\n");
    assert.equal(snapshot.backgroundTasks.find((task) => task.id === "failed-task")?.status, "failed");
    assert.equal(snapshot.backgroundTasks.find((task) => task.id === "stopped-task")?.kind, "monitor");
    assert.equal(claude.snapshot(2).thread!.hasOlderItems, true);
    await claude.updateSettings({ model: "opus" });
    assert.equal(claude.snapshot().thread?.model, "opus");
    await assert.rejects(claude.updateSettings({ model: "invalid" }), /Invalid model/);
  } finally { await claude.stop(); }
});

test("server queue and native mid-turn steering remain separate, interrupt ends a turn", async () => {
  const cwd = temp();
  const log = join(cwd, "calls"); process.env.FAKE_CLAUDE_CALLS = log;
  const claude = runtime(cwd);
  const queue = new PromptQueue(claude, () => {});
  try {
    await claude.start();
    await queue.submit("hold");
    await queue.submit("after", "queue");
    assert.equal(queue.entries.length, 1);
    const steered = await queue.submit("steer now", "steer");
    assert.equal(steered.disposition, "steered");
    await until(() => queue.entries.length === 0 && claude.snapshot().thread?.status === "idle");
    const calls = readFileSync(log, "utf8").trim().split("\n").map((line) => JSON.parse(line));
    assert.deepEqual(calls.filter((call) => call.type === "user").map((call) => call.message.content[0].text), ["hold", "steer now", "after"]);
    await claude.prompt("hold"); await claude.interrupt();
    await until(() => claude.snapshot().thread?.status === "idle");
    assert.equal(claude.snapshot().thread!.turns.at(-1)!.status, "interrupted");
    assert.ok(calls.every((call) => call.priority === undefined));
  } finally { await claude.stop(); delete process.env.FAKE_CLAUDE_CALLS; }
});

test("resume maps JSONL user/assistant/tools without stdout replay; missing/rejected history never creates a session", async () => {
  const cwd = temp(); const configDir = join(cwd, "config"); const stateFile = join(cwd, "state");
  process.env.FAKE_CLAUDE_CONFIG = configDir;
  const first = runtime(cwd, { configDir, stateFile });
  try { await first.start(); await first.prompt("remember"); await until(() => first.snapshot().thread?.status === "idle"); } finally { await first.stop(); }
  const second = runtime(cwd, { configDir, stateFile });
  try {
    await second.start();
    assert.equal(second.snapshot().thread?.id, loadThreadId(stateFile));
    assert.equal(second.snapshot().backgroundTasks.length, 0);
    assert.deepEqual(second.snapshot().thread!.turns[0].items.map((item) => item.kind), ["userMessage", "reasoning", "commandExecution", "fileChange", "fileChange", "fileChange", "plan", "toolCall", "agentMessage"]);
    assert.equal(second.snapshot().thread!.turns[0].status, "completed");
  } finally { await second.stop(); delete process.env.FAKE_CLAUDE_CONFIG; }
  const missing = runtime(cwd, { configDir, sessionId: "missing" });
  await assert.rejects(missing.start(), /Failed to resume Claude session missing; no new session was created/);
  const rejectedPath = join(configDir, "projects", cwd.replace(/[^a-zA-Z0-9]/g, "-"), "reject.jsonl");
  mkdirSync(dirname(rejectedPath), { recursive: true }); writeFileSync(rejectedPath, "");
  const rejected = runtime(cwd, { configDir, sessionId: "reject", stateFile });
  const previousId = loadThreadId(stateFile);
  await assert.rejects(rejected.start(), /Cannot resume this session/);
  assert.equal(loadThreadId(stateFile), previousId);
});

test("completed tasks are bounded to 100; running tasks survive pruning", async () => {
  const claude = runtime(temp());
  try {
    await claude.start(); await claude.prompt("many-tasks");
    await until(() => claude.snapshot().thread?.status === "idle");
    const tasks = claude.snapshot().backgroundTasks;
    assert.equal(tasks.length, 101);
    assert.equal(tasks.find((task) => task.id === "still-running")?.status, "running");
    assert.ok(!tasks.some((task) => task.id === "t0"));
  } finally { await claude.stop(); }
});

test("Claude content mapping keeps text and new/old diffs literal", () => {
  const item = mapClaudeBlock({ type: "tool_use", name: "Edit", input: { file_path: "/workspace/test.txt", old_string: "<b>old</b>", new_string: "<script>new</script>" } }, "edit", "/workspace");
  assert.equal(item.changes![0].path, "test.txt");
  assert.match(item.changes![0].diff, /\+<script>new<\/script>/);
});

test("missing Claude executable fails promptly without saving a session", async () => {
  const cwd = temp(); const stateFile = join(cwd, "state");
  const claude = new ClaudeRuntime(join(cwd, "missing-claude"), cwd, { stateFile });
  await assert.rejects(claude.start(), /Failed to start Claude.*ENOENT/);
  assert.equal(loadThreadId(stateFile), undefined);
  assert.equal(claude.snapshot().ready, false);
});
