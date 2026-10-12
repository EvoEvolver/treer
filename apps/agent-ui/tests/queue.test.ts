import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { test } from "node:test";
import { mkdtemp, writeFile, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { HarnessRuntime, ThreadState } from "../src/runtime.js";
import type { BackgroundTask } from "../src/model.js";
import { PromptQueue, promptMode } from "../src/prompt-queue.js";
import { taskOutput } from "../src/task-output.js";

class Fake extends EventEmitter {
  harness = { id: "fake", name: "Fake", capabilities: { steer: true, backgroundTasks: false } };
  calls: string[] = [];
  thread = { status: "idle" } as ThreadState;
  snapshot() { return { ready: true, thread: this.thread, models: [], backgroundTasks: [] }; }
  async prompt(text: string) { this.calls.push(text); this.thread.status = "running"; this.emit("state"); }
  idle() { this.thread.status = "idle"; this.emit("state"); }
}

test("server queues FIFO across turns, supports deletion, and steers only when capable", async () => {
  const runtime = new Fake();
  const queue = new PromptQueue(runtime as unknown as HarnessRuntime, () => {});
  await queue.submit("first", "queue");
  const second = await queue.submit("second", "queue");
  const deleted = await queue.submit("deleted", "queue");
  assert.deepEqual(runtime.calls, ["first"]);
  assert.equal(queue.remove(deleted.id!), true);
  assert.equal(queue.remove("missing"), false);
  assert.equal((await queue.submit("steering")).disposition, "steered");
  runtime.idle();
  await new Promise((done) => setImmediate(done));
  assert.deepEqual(runtime.calls, ["first", "steering", "second"]);
  assert.equal(queue.entries.length, 0);
  assert.equal(second.disposition, "queued");
  runtime.harness.capabilities.steer = false;
  const fallback = await queue.submit("unsupported", "steer");
  assert.equal(fallback.downgraded, true);
  assert.equal(fallback.mode, "queue");
  assert.equal(queue.entries[0].text, "unsupported");
  assert.throws(() => promptMode("invalid"), /mode must/);
});

test("dispatch lock prevents overlapping asynchronous queued sends", async () => {
  const runtime = new Fake();
  let finish!: () => void;
  runtime.prompt = async (text) => { runtime.calls.push(text); await new Promise<void>((done) => { finish = done; }); runtime.thread.status = "running"; };
  const queue = new PromptQueue(runtime as unknown as HarnessRuntime, () => {});
  const first = queue.submit("first", "queue");
  await queue.submit("second", "queue");
  assert.deepEqual(runtime.calls, ["first"]);
  finish(); await first;
  assert.equal(queue.entries[0].text, "second");
});

test("task output is registered, bounded, tailed, and rejects traversal and external symlinks", async () => {
  const root = await mkdtemp(join(tmpdir(), "agent-ui-output-"));
  const outside = await mkdtemp(join(tmpdir(), "agent-ui-outside-"));
  const file = join(root, "output");
  await writeFile(file, "a".repeat(70_000) + "END");
  const task: BackgroundTask = { id: "t1", kind: "shell", description: "test", status: "completed", startedAt: null, endedAt: null, exitCode: 0, summary: null, outputFile: file };
  const result = await taskOutput(task, [root]);
  assert.equal(Buffer.byteLength(result.content), 65536);
  assert.equal(result.truncated, true);
  assert.equal((await taskOutput(task, [root], "3")).content, "END");
  await writeFile(join(outside, "secret"), "secret");
  await symlink(join(outside, "secret"), join(root, "link"));
  await assert.rejects(taskOutput({ ...task, outputFile: join(root, "link") }, [root]), /outside trusted/);
  await assert.rejects(taskOutput({ ...task, outputFile: join(outside, "secret") }, [root]), /outside trusted/);
  await assert.rejects(taskOutput(task, [root], "-1"), /positive/);
  await assert.rejects(taskOutput({ ...task, outputFile: undefined }, [root]), /not available/);
});
