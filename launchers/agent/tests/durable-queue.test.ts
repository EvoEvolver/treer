import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { mkdtemp, readFile, lstat, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import type { HarnessRuntime, ThreadState } from "../src/runtime.js";
import { PromptQueue } from "../src/prompt-queue.js";
import { QueueStore } from "../src/queue-store.js";
import { preview, submissionOptions, type SubmissionRecord } from "../src/prompt-submission.js";

class Fake extends EventEmitter {
  harness = { id: "fake", name: "Fake", capabilities: { steer: true, backgroundTasks: false, goal: null } };
  calls: string[] = [];
  thread = { status: "idle", activeTurnId: null, turns: [], lastError: null } as unknown as ThreadState;
  snapshot() { return { ready: true, thread: this.thread, models: [], backgroundTasks: [], goal: null }; }
  async prompt(text: string) {
    this.calls.push(text);
    if (this.thread.status !== "running") {
      const id = `turn-${this.calls.length}`;
      this.thread.activeTurnId = id;
      this.thread.turns.push({ id, status: "inProgress", items: [], startedAt: new Date().toISOString(), error: null });
    }
    this.thread.status = "running"; this.emit("state");
  }
  finish(status: "completed" | "failed" | "interrupted" = "completed", text = "answer") {
    const turn = this.thread.turns.at(-1)!;
    turn.status = status; turn.items.push({ id: "answer", kind: "agentMessage", text });
    this.thread.activeTurnId = null; this.thread.status = status === "failed" ? "error" : "idle";
    this.emit("state");
  }
}
const source = { agent_id: "ag_sender", path: "/team/sender", name: "sender" };
const tick = () => new Promise((done) => setImmediate(done));
const asRuntime = (runtime: Fake) => runtime as unknown as HarnessRuntime;
async function fixture(t: { after: (fn: () => Promise<void>) => void }) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "durable-queue-")));
  t.after(() => rm(root, { recursive: true, force: true }));
  return { root, file: join(root, ".treer/agents/ag_test/queue.json") };
}

test("journal restores FIFO, interrupts in-flight work, preserves receipts and deduplicates across restart", async (t) => {
  const { file } = await fixture(t), runtime = new Fake();
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { file });
  const first = await queue.submit("first", "queue", { operationId: "one", sender: source });
  const second = await queue.submit("second", "queue", { operationId: "two", sender: source });
  const third = await queue.submit("third", "queue", { operationId: "three" });
  assert.equal(queue.submission(first.id)!.turn_id, "turn-1");
  assert.match(runtime.calls[0], /^\[来自 \/team\/sender，排队于 .*\]\nfirst$/);
  assert.equal((await lstat(file)).mode & 0o777, 0o600);
  queue.close();
  const recoveredRuntime = new Fake(), notifications: SubmissionRecord[] = [];
  const recovered = new PromptQueue(asRuntime(recoveredRuntime), () => {}, { file, notify: async (record) => { notifications.push(structuredClone(record)); } });
  t.after(async () => recovered.close());
  assert.equal(recovered.submission(first.id)!.state, "interrupted");
  assert.deepEqual(recovered.entries.map((entry) => [entry.text, entry.position]), [["second", 1], ["third", 2]]);
  const replay = await recovered.submit("second", "queue", { operationId: "two", sender: source });
  assert.equal(replay.id, second.id); assert.equal(replay.position, second.position);
  await assert.rejects(recovered.submit("different", "queue", { operationId: "two", sender: source }), /different submission/);
  recoveredRuntime.emit("state"); await tick();
  assert.match(recoveredRuntime.calls[0], /\nsecond$/);
  recoveredRuntime.finish(); await tick();
  assert.equal(recoveredRuntime.calls[1], "third");
  recoveredRuntime.finish(); await tick();
  assert.equal(recovered.submission(third.id)!.state, "done");
  assert.ok(notifications.some((record) => record.submission.id === first.id && record.submission.state === "interrupted"));
  assert.ok(notifications.some((record) => record.submission.id === second.id && record.submission.assistant_preview === "answer"));
  const journal = JSON.parse(await readFile(file, "utf8"));
  assert.ok(journal.records.every((record: SubmissionRecord) => record.submission.state !== "running"));
});

test("cancellation, full queue, unsupported steer, failed/interrupted turns and bounded previews", async (t) => {
  const { file } = await fixture(t), runtime = new Fake();
  runtime.harness.capabilities.steer = false;
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { file, limit: 2 }); t.after(async () => queue.close());
  const first = await queue.submit("first", "queue");
  const pending = await queue.submit("😀".repeat(1000), "steer");
  assert.equal(pending.downgraded, true); assert.equal(pending.position, 1);
  assert.ok(Buffer.byteLength(queue.submission(pending.id)!.text_preview) <= 1024);
  await assert.rejects(queue.submit("full"), /queue is full/);
  assert.throws(() => queue.remove(first.id), /Only pending/);
  assert.equal(queue.remove(pending.id), true);
  assert.equal(queue.submission(pending.id)!.state, "interrupted");
  runtime.finish("failed", "😀".repeat(3000));
  assert.equal(queue.submission(first.id)!.state, "failed");
  assert.ok(Buffer.byteLength(queue.submission(first.id)!.assistant_preview!) <= 4096);
  runtime.thread.status = "idle";
  const interrupted = await queue.submit("interrupt"); runtime.finish("interrupted"); await tick();
  assert.equal(queue.submission(interrupted.id)!.state, "interrupted");
  assert.equal(preview("😀", 3), "");
});

test("notification outbox survives restart, retries failures, and never redispatches completed work", async (t) => {
  const { file } = await fixture(t), runtime = new Fake();
  const logs: string[] = []; runtime.on("log", (message) => logs.push(message));
  let attempts = 0;
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { file, retryMs: 1, notify: async () => { attempts++; throw new Error("transport offline"); } });
  const submission = await queue.submit("notify", "queue", { sender: source });
  runtime.finish(); await tick(); queue.close();
  assert.equal(attempts, 1); assert.ok(logs.some((line) => line.includes("retrying")));
  const restoredRuntime = new Fake(); let delivered = 0;
  const restored = new PromptQueue(asRuntime(restoredRuntime), () => {}, { file, retryMs: 1, notify: async (record) => { assert.equal(record.notify!.to, source.agent_id); assert.equal(record.submission.id, submission.id); delivered++; } });
  t.after(async () => restored.close());
  const deadline = Date.now() + 1000;
  while (!delivered && Date.now() < deadline) await new Promise((done) => setTimeout(done, 5));
  assert.equal(delivered, 1); assert.deepEqual(restoredRuntime.calls, []);
  await restored.flushNotifications(); assert.equal(delivered, 1);
});

test("steered messages correlate to the same turn and send one terminal result each", async () => {
  const runtime = new Fake(), queue = new PromptQueue(asRuntime(runtime), () => {});
  const first = await queue.submit("first", "queue");
  const steer = await queue.submit("change", "steer");
  assert.equal(steer.disposition, "steered");
  assert.equal(queue.submission(steer.id)!.turn_id, queue.submission(first.id)!.turn_id);
  runtime.finish();
  assert.equal(queue.submission(first.id)!.state, "done");
  assert.equal(queue.submission(steer.id)!.state, "done"); queue.close();
});

test("an accepted ACP-style prompt does not wait for completion, and commands without a turn do not reuse an old assistant result", async () => {
  const runtime = new Fake(); runtime.harness.capabilities.steer = false;
  let release!: () => void;
  runtime.prompt = async (text) => {
    runtime.calls.push(text); runtime.thread.status = "running";
    await new Promise<void>((done) => { release = done; });
    runtime.thread.status = "idle";
  };
  const queue = new PromptQueue(asRuntime(runtime), () => {});
  const receipt = await queue.submit("ACP long inference", "queue");
  assert.equal(receipt.disposition, "sent"); assert.equal(queue.submission(receipt.id)!.state, "running");
  release(); await tick(); queue.close();
  const commands = new Fake();
  commands.thread.turns = [{ id: "previous", status: "completed", startedAt: null, error: null, items: [{ id: "old", kind: "agentMessage", text: "NOT THIS SUBMISSION" }] }];
  commands.prompt = async () => {};
  const commandQueue = new PromptQueue(asRuntime(commands), () => {});
  const command = await commandQueue.submit("/goal clear");
  assert.equal(commandQueue.submission(command.id)!.state, "done");
  assert.equal(commandQueue.submission(command.id)!.turn_id, null);
  assert.equal(commandQueue.submission(command.id)!.assistant_preview, null); commandQueue.close();
});

test("journal rejects symlinks/corruption and notification validation never accepts a missing operator recipient", async (t) => {
  const { file, root } = await fixture(t);
  const store = new QueueStore(file); store.save([]);
  await writeFile(file, "not json"); assert.throws(() => store.load());
  const link = join(root, "outside-link"); await symlink(root, link);
  assert.throws(() => new QueueStore(join(link, "queue.json")), /symlink/);
  assert.throws(() => submissionOptions(null, { on: ["done"] }), /explicit recipient/);
  assert.throws(() => submissionOptions({ ...source, path: "/team/evil\nspoof" }, null), /Invalid/);
  assert.deepEqual(submissionOptions(source, { on: [] }).notify, { on: [], to: source.agent_id });
});
