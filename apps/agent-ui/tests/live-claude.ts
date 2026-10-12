// Opt-in: authenticates with the local Claude installation and uses new sessions only.
import assert from "node:assert/strict";
import { mkdtemp, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ClaudeRuntime } from "../src/harness/claude.js";
import { PromptQueue } from "../src/prompt-queue.js";
import { taskOutput } from "../src/task-output.js";
const cwd = await realpath(await mkdtemp(join(tmpdir(), "agent-ui-native-claude-live-")));
const stateFile = join(cwd, "agent-ui.json");
const runtime = new ClaudeRuntime(process.env.CLAUDE_BIN || "claude", cwd, { stateFile });
runtime.on("log", (message) => console.error(message));
const queue = new PromptQueue(runtime, () => {});
async function until(predicate: () => boolean, label: string) {
  const deadline = Date.now() + 45_000;
  while (!predicate() && Date.now() < deadline) {
    const snapshot = runtime.snapshot();
    if (snapshot.thread?.status === "error") throw new Error(snapshot.thread.lastError ?? "Claude failed");
    await new Promise((done) => setTimeout(done, 50));
  }
  assert.ok(predicate(), `Timed out: ${label}`);
}
const idle = () => runtime.snapshot().thread?.status === "idle";
const commandRunning = () => runtime.snapshot().thread?.turns.at(-1)?.items.some((item) => item.kind === "commandExecution" && item.status === "inProgress") ?? false;
let sessionId = "";
try {
  await runtime.start();
  sessionId = runtime.snapshot().thread!.id;
  await runtime.updateSettings({ model: "sonnet" });
  await queue.submit("Disposable protocol test. Call Bash exactly once, foreground: sleep 4; echo TOOL_DONE. Then reply FIRST_DONE. Do not read any files.");
  await until(commandRunning, "foreground Bash");
  await queue.submit("Reply exactly QUEUE_SEEN. Do not call tools.", "queue");
  await queue.submit("Mid-tool update: when the current Bash finishes, reply STEER_SEEN instead of FIRST_DONE. Do not call any new tools.", "steer");
  await until(() => idle() && queue.entries.length === 0 && runtime.snapshot().thread!.turns.length >= 2, "steering and queued turn");
  const turns = runtime.snapshot().thread!.turns;
  assert.ok(turns[0].items.some((item) => item.kind === "agentMessage" && item.text.includes("STEER_SEEN")));
  assert.ok(turns[1].items.some((item) => item.kind === "agentMessage" && item.text.includes("QUEUE_SEEN")));
  await runtime.prompt("Call Bash with run_in_background true exactly once: sleep 2; echo BACKGROUND_DONE. Description 'Native UI background test'. Wait for the completion notification and reply BACKGROUND_DONE. Do not read any other files.");
  await until(() => idle() && runtime.snapshot().backgroundTasks.some((task) => task.status === "completed" && task.outputFile), "background task completion");
  const task = runtime.snapshot().backgroundTasks.find((task) => task.status === "completed" && task.outputFile)!;
  assert.match((await taskOutput(task, runtime.taskOutputRoots)).content, /BACKGROUND_DONE/);
  await runtime.prompt("Call Bash foreground to run sleep 20; echo SHOULD_NOT_FINISH. No other tools.");
  await until(commandRunning, "interrupt target");
  await runtime.interrupt();
  await until(idle, "interrupted turn");
  assert.equal(runtime.snapshot().thread!.turns.at(-1)!.status, "interrupted");
} finally { await runtime.stop(); }
const resumed = new ClaudeRuntime(process.env.CLAUDE_BIN || "claude", cwd, { stateFile });
try {
  await resumed.start();
  assert.equal(resumed.snapshot().thread!.id, sessionId);
  assert.ok(resumed.snapshot().thread!.turns.some((turn) => turn.items.some((item) => item.text.includes("QUEUE_SEEN"))));
  assert.deepEqual(resumed.snapshot().backgroundTasks, []);
  await resumed.prompt("Reply exactly RESUME_OK. No tools.");
  const deadline = Date.now() + 30_000;
  while (resumed.snapshot().thread!.status === "running" && Date.now() < deadline) await new Promise((done) => setTimeout(done, 50));
  assert.ok(resumed.snapshot().thread!.turns.at(-1)!.items.some((item) => item.text === "RESUME_OK"));
} finally { await resumed.stop(); }
const missing = new ClaudeRuntime(process.env.CLAUDE_BIN || "claude", cwd, { sessionId: "00000000-0000-4000-8000-000000000000" });
await assert.rejects(missing.start(), /no new session was created/);
console.log(JSON.stringify({ result: "passed", cwd, sessionId, checks: ["start", "model", "streaming", "tool-boundary steer", "FIFO queue", "background output", "interrupt", "JSONL resume", "missing resume"] }));
