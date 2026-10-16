import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { spawn } from "node:child_process";
import { PassThrough } from "node:stream";
import { mkdtemp, readFile, lstat, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import type { HarnessImage, HarnessRuntime, ThreadState } from "../src/runtime.js";
import { PromptQueue } from "../src/prompt-queue.js";
import { QueueStore } from "../src/queue-store.js";
import { completionKey, preview, submissionOptions, type SubmissionRecord } from "../src/prompt-submission.js";
import { sendNotification, submissionNotification } from "../src/prompt-notify.js";

class Fake extends EventEmitter {
  harness = { id: "fake", name: "Fake", capabilities: { steer: true, backgroundTasks: false, goal: null } };
  calls: string[] = [];
  images: HarnessImage[][] = [];
  thread = { status: "idle", activeTurnId: null, turns: [], lastError: null } as unknown as ThreadState;
  snapshot() { return { ready: true, thread: this.thread, models: [], backgroundTasks: [], goal: null }; }
  async prompt(text: string, images: HarnessImage[] = []) {
    this.calls.push(text);
    this.images.push(images);
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
test("image IDs persist across restart, dedupe with the prompt and resolve only in their session", async t => {
  const { file } = await fixture(t), runtime = new Fake();
  runtime.thread.id = "image-session";
  runtime.thread.status = "running";
  const id = `img_${"b".repeat(32)}`;
  const image: HarnessImage = { id, mime: "image/png", size: 10, path: "/private/image.png", data: "secret-base64" };
  let retained = false;
  const attachments = {
    resolve: (ids: string[], session: string) => { assert.deepEqual(ids, [id]); assert.equal(session, "image-session"); return [image]; },
    retain: () => { retained = true; },
  };
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { file, attachments });
  const receipt = await queue.submit("", "queue", { operationId: "images-once", attachmentIds: [id] });
  assert.equal(retained, true);
  assert.equal(runtime.calls.length, 0);
  const journal = await readFile(file, "utf8");
  assert.equal(journal.includes(image.path), false); assert.equal(journal.includes(image.data), false);
  assert.equal((await queue.submit("", "queue", { operationId: "images-once", attachmentIds: [id] })).duplicate, true);
  await assert.rejects(queue.submit("", "queue", { operationId: "images-once", attachmentIds: [`img_${"c".repeat(32)}`] }), /different submission/);
  queue.close();
  const recoveredRuntime = new Fake(); recoveredRuntime.thread.id = "image-session";
  const recovered = new PromptQueue(asRuntime(recoveredRuntime), () => {}, { file, attachments });
  t.after(async () => recovered.close());
  recoveredRuntime.emit("state"); await tick();
  assert.deepEqual(recoveredRuntime.images, [[image]]);
  assert.equal(recovered.submission(receipt.id)!.state, "running");
  recoveredRuntime.finish(); await tick();
  assert.equal(recovered.submission(receipt.id)!.state, "done");
});

test("a queued image cannot dispatch after the native session changes", async t => {
  const { file } = await fixture(t), runtime = new Fake();
  runtime.thread.id = "first"; runtime.thread.status = "running";
  const id = `img_${"b".repeat(32)}`;
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { file, attachments: { resolve: () => [], retain: () => {} } });
  t.after(async () => queue.close());
  const receipt = await queue.submit("look", "queue", { attachmentIds: [id] });
  runtime.thread.id = "other"; runtime.thread.status = "idle"; runtime.emit("state"); await tick();
  assert.equal(runtime.calls.length, 0);
  assert.equal(queue.submission(receipt.id)!.state, "failed");
});
const source = { agent_id: "ag_sender", path: "/team/sender", name: "sender" };
const tick = () => new Promise((done) => setImmediate(done));
const asRuntime = (runtime: Fake) => runtime as unknown as HarnessRuntime;
for (const notify of [undefined, null]) for (const terminal of ["completed", "failed", "interrupted", "exit", "restart"] as const) test(`sender with ${notify === null ? "null" : "omitted"} notify stays silent on ${terminal}`, async (t) => {
  assert.equal(submissionOptions(source, notify).notify, null);
  const { file } = await fixture(t);
  const runtime = new Fake();
  const sent: SubmissionRecord[] = [];
  const options = { file, notify: async (record: SubmissionRecord) => { sent.push(structuredClone(record)); } };
  let queue = new PromptQueue(asRuntime(runtime), () => {}, options);
  t.after(async () => queue.close());
  const first = await queue.submit("plain", "queue", { sender: source, notify });
  const steer = await queue.submit("steer without notify", "steer", { sender: source, notify });
  assert.equal(steer.disposition, "steered");
  if (terminal === "restart") {
    queue.close();
    queue = new PromptQueue(asRuntime(new Fake()), () => {}, options);
  } else if (terminal === "exit") runtime.emit("exit");
  else runtime.finish(terminal);
  await tick(); await queue.flushNotifications();
  const expected = terminal === "completed" ? "done" : terminal === "exit" || terminal === "restart" ? "interrupted" : terminal;
  for (const id of [first.id, steer.id]) assert.equal(queue.submission(id)!.state, expected);
  assert.deepEqual(sent, []);
  const journal = JSON.parse(await readFile(file, "utf8"));
  assert.equal(journal.records.length, 2);
  for (const record of journal.records) {
    assert.equal(record.notify, null);
    assert.equal(record.notification, null);
  }
});
async function fixture(t: { after: (fn: () => Promise<void>) => void }) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "durable-queue-")));
  t.after(() => rm(root, { recursive: true, force: true }));
  return { root, file: join(root, ".treer/agents/ag_test/queue.json") };
}

test("journal restores FIFO, interrupts in-flight work, preserves receipts and deduplicates across restart", async (t) => {
  const { file } = await fixture(t), runtime = new Fake();
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { file });
  const first = await queue.submit("first", "queue", { operationId: "one", sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  const second = await queue.submit("second", "queue", { operationId: "two", sender: source, notify: { on: ["done", "failed", "interrupted"] } });
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
  const replay = await recovered.submit("second", "queue", { operationId: "two", sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  assert.equal(replay.id, second.id); assert.equal(replay.position, second.position);
  await assert.rejects(recovered.submit("different", "queue", { operationId: "two", sender: source, notify: { on: ["done", "failed", "interrupted"] } }), /different submission/);
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
  const submission = await queue.submit("notify", "queue", { sender: source, notify: { on: ["done", "failed", "interrupted"] } });
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

for (const outcome of ["completed", "interrupted", "failed"] as const) test(`one ${outcome} notification per turn/recipient contains all opted-in steer submissions`, async (t) => {
  const { file } = await fixture(t), runtime = new Fake(), sent: SubmissionRecord[] = [];
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { file, notify: async (record) => {
    // The complete frozen envelope must be on disk before the transport sees it.
    const saved = JSON.parse(await readFile(file, "utf8")).records.find((entry: SubmissionRecord) => entry.submission.id === record.submission.id);
    assert.deepEqual(saved.notification.batch, record.notification!.batch);
    sent.push(structuredClone(record));
  } });
  t.after(async () => queue.close());
  const ids = [];
  ids.push((await queue.submit("first", "queue", { sender: source, notify: { on: ["done", "failed", "interrupted"] } })).id);
  for (let i = 0; i < 5; i++) ids.push((await queue.submit(`steer ${i}`, "steer", { sender: source, notify: { on: ["done", "failed", "interrupted"] } })).id);
  const other = await queue.submit("other recipient", "steer", { sender: source, notify: { to: "ag_other", on: ["done", "failed", "interrupted"] } });
  const disabled = await queue.submit("no notify", "steer", { sender: source, notify: { on: [] } });
  const selected = await queue.submit("failed only", "steer", { sender: source, notify: { on: ["failed"] } });
  await queue.flushNotifications(); assert.equal(sent.length, 0);
  runtime.finish(outcome); await tick(); await queue.flushNotifications();
  // flushNotifications is reentrant-safe; wait for the currently active send.
  const deadline = Date.now() + 5000;
  while (sent.length < 2 && Date.now() < deadline) await new Promise((done) => setTimeout(done, 5));
  assert.equal(sent.length, 2);
  const batch = sent.find((record) => record.notify!.to === source.agent_id)!.notification!.batch!;
  assert.deepEqual(batch.submission_ids, outcome === "failed" ? [...ids, selected.id] : ids);
  assert.equal(batch.status, outcome === "completed" ? "done" : outcome);
  assert.equal(batch.assistant_preview, "answer");
  assert.deepEqual(sent.find((record) => record.notify!.to === "ag_other")!.notification!.batch!.submission_ids, [other.id]);
  assert.ok(!batch.submission_ids.includes(disabled.id));
  for (const id of ids) assert.equal(queue.submission(id)!.state, batch.status);
  // Duplicated and even contradictory native terminal events cannot resend.
  runtime.finish("completed", "later duplicate"); runtime.emit("state"); runtime.emit("exit");
  await tick(); await queue.flushNotifications(); assert.equal(sent.length, 2);
});

test("a frozen completion retries across restart with identical membership/payload/key, without rerunning inference", async (t) => {
  const { file } = await fixture(t), runtime = new Fake(), sent: SubmissionRecord[] = [];
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { file, retryMs: 60_000, notify: async (record) => {
    sent.push(structuredClone(record)); throw new Error("send succeeded but its acknowledgement was lost");
  } });
  const first = await queue.submit("one", "queue", { sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  const second = await queue.submit("two", "steer", { sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  runtime.finish(); await tick(); queue.close();
  assert.equal(sent.length, 1);
  assert.deepEqual(sent[0].notification!.batch!.submission_ids, [first.id, second.id]);
  const journal = JSON.parse(await readFile(file, "utf8"));
  for (const record of journal.records) if (record.notification) record.notification.nextAttempt = 0;
  await writeFile(file, JSON.stringify(journal)); // Simulate the persisted retry deadline elapsing.
  const restoredRuntime = new Fake();
  const recovered = new PromptQueue(asRuntime(restoredRuntime), () => {}, { file, notify: async (record) => { sent.push(structuredClone(record)); } });
  t.after(async () => recovered.close());
  await tick(); await recovered.flushNotifications();
  assert.equal(sent.length, 2);
  assert.deepEqual(sent[1].notification!.batch, sent[0].notification!.batch);
  assert.deepEqual(restoredRuntime.calls, []);
  const persisted = JSON.parse(await readFile(file, "utf8"));
  assert.ok(persisted.records.every((record: SubmissionRecord) => record.notification!.delivered));
  await recovered.flushNotifications(); assert.equal(sent.length, 2);
  recovered.close();
  const again = new PromptQueue(asRuntime(new Fake()), () => {}, { file, notify: async () => { assert.fail("delivered batch must not be replayed"); } });
  t.after(async () => again.close()); await tick();
});

test("ACP-style sequential turns and restarted turn counters remain separate notifications", async (t) => {
  const { file } = await fixture(t), runtime = new Fake(), batches: SubmissionRecord[] = [];
  runtime.harness.capabilities.steer = false;
  const options = { file, notify: async (record: SubmissionRecord) => { batches.push(structuredClone(record)); } };
  const queue = new PromptQueue(asRuntime(runtime), () => {}, options);
  const first = await queue.submit("first", "queue", { sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  const second = await queue.submit("not steerable", "steer", { sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  assert.equal(second.downgraded, true);
  runtime.finish(); await tick(); runtime.finish(); await tick(); queue.close();
  assert.equal(batches.length, 2);
  assert.deepEqual(batches.map((record) => record.notification!.batch!.submission_ids), [[first.id], [second.id]]);
  const restarted = new Fake(); restarted.harness.capabilities.steer = false;
  const recovered = new PromptQueue(asRuntime(restarted), () => {}, options); t.after(async () => recovered.close());
  const third = await recovered.submit("new process uses turn-1 again", "queue", { sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  restarted.finish(); await tick();
  assert.equal(batches.length, 3);
  assert.equal(batches[0].submission.turn_id, batches[2].submission.turn_id);
  assert.notEqual(completionKey(batches[0]), completionKey(batches[2]));
  assert.deepEqual(batches[2].notification!.batch!.submission_ids, [third.id]);
});

test("cancelled queued work and local commands without turn IDs keep independent terminal messages", async (t) => {
  const runtime = new Fake(), sent: SubmissionRecord[] = [];
  runtime.thread.status = "running"; runtime.thread.activeTurnId = "external";
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { notify: async (record) => { sent.push(structuredClone(record)); } });
  t.after(async () => queue.close());
  const ids = [];
  for (let i = 0; i < 2; i++) ids.push((await queue.submit("cancel", "queue", { sender: source, notify: { on: ["done", "failed", "interrupted"] } })).id);
  for (const id of ids) queue.remove(id);
  await tick(); await queue.flushNotifications();
  assert.equal(sent.length, 2);
  for (const record of sent) {
    assert.equal(record.notification!.batch!.turn_id, null);
    assert.deepEqual(record.notification!.batch!.submission_ids, [record.submission.id]);
    assert.equal(record.notification!.batch!.key, `prompt-submission/${record.submission.id}/terminal`);
  }
});

test("an in-flight steer failure waits for the actual turn terminal state before notification", async (t) => {
  const runtime = new Fake(), sent: SubmissionRecord[] = [];
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { notify: async (record) => { sent.push(structuredClone(record)); } });
  t.after(async () => queue.close());
  const first = await queue.submit("first", "queue", { sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  runtime.prompt = async () => { throw new Error("steer rejected"); };
  const steer = await queue.submit("change", "steer", { sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  assert.equal(queue.submission(steer.id)!.state, "failed");
  await queue.flushNotifications(); assert.equal(sent.length, 0);
  runtime.finish(); await tick();
  assert.equal(sent.length, 1);
  assert.equal(sent[0].notification!.batch!.status, "done");
  assert.deepEqual(sent[0].notification!.batch!.submission_ids, [first.id, steer.id]);
  assert.equal(queue.submission(steer.id)!.state, "failed"); // Individual receipts remain authoritative for dispatch errors.
});

test("harness exit finishes a turn once even if an outstanding prompt rejects later", async (t) => {
  const runtime = new Fake(), sent: SubmissionRecord[] = [];
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { notify: async (record) => { sent.push(structuredClone(record)); } });
  t.after(async () => queue.close());
  await queue.submit("first", "queue", { sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  let reject!: (error: Error) => void;
  runtime.prompt = async () => new Promise<void>((_resolve, fail) => { reject = fail; });
  const steering = queue.submit("in-flight", "steer", { sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  runtime.emit("exit"); await tick();
  assert.equal(sent.length, 1); assert.equal(sent[0].notification!.batch!.status, "interrupted");
  assert.equal(sent[0].notification!.batch!.submission_ids.length, 2);
  reject(new Error("process ended")); await steering; await tick();
  assert.equal(sent.length, 1);
});

test("legacy delivered siblings suppress pending duplicates; unbatched pending siblings are coalesced", async (t) => {
  const { file } = await fixture(t), runtime = new Fake();
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { file });
  const first = await queue.submit("first", "queue", { sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  const second = await queue.submit("steer", "steer", { sender: source, notify: { on: ["done", "failed", "interrupted"] } });
  runtime.finish(); queue.close();
  const legacy = JSON.parse(await readFile(file, "utf8"));
  for (const record of legacy.records) delete record.turnScope;
  await writeFile(file, JSON.stringify(legacy));
  const sent: SubmissionRecord[] = [];
  const recovered = new PromptQueue(asRuntime(new Fake()), () => {}, { file, notify: async (record) => { sent.push(structuredClone(record)); } });
  await tick(); recovered.close(); assert.equal(sent.length, 1);
  assert.deepEqual(sent[0].notification!.batch!.submission_ids, [first.id, second.id]);
  legacy.records[0].notification.delivered = true;
  await writeFile(file, JSON.stringify(legacy));
  const upgraded = new PromptQueue(asRuntime(new Fake()), () => {}, { file, notify: async () => { assert.fail("already-delivered turn must not be resent"); } });
  t.after(async () => upgraded.close()); await tick();
  assert.ok(JSON.parse(await readFile(file, "utf8")).records.every((record: SubmissionRecord) => record.notification!.delivered));
});

test("journal rejects forged completion membership and recipient", async (t) => {
  const { file } = await fixture(t), runtime = new Fake();
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { file, retryMs: 60_000, notify: async () => { throw new Error("offline"); } });
  await queue.submit("first", "queue", { sender: source, notify: { on: ["done", "failed", "interrupted"] } }); runtime.finish(); await tick(); queue.close();
  const journal = JSON.parse(await readFile(file, "utf8")), batch = journal.records[0].notification.batch;
  batch.submission_ids.push("sub_missing"); await writeFile(file, JSON.stringify(journal));
  assert.throws(() => new QueueStore(file).load(), /completion membership/);
  batch.submission_ids.pop(); batch.recipient = "ag_unrelated"; await writeFile(file, JSON.stringify(journal));
  assert.throws(() => new QueueStore(file).load(), /completion batch/);
});

test("all 512 completion IDs fit Core's message limit, escaped previews are bounded, and overflow steers are rejected before dispatch", async (t) => {
  const runtime = new Fake(), sent: SubmissionRecord[] = [];
  const queue = new PromptQueue(asRuntime(runtime), () => {}, { limit: 1024, notify: async (record) => { sent.push(structuredClone(record)); } });
  t.after(async () => queue.close());
  const ids = [(await queue.submit("first", "queue", { sender: source, notify: { on: ["done", "failed", "interrupted"] } })).id];
  for (let i = 1; i < 512; i++) ids.push((await queue.submit(`steer ${i}`, "steer", { sender: source, notify: { on: ["done", "failed", "interrupted"] } })).id);
  await assert.rejects(queue.submit("overflow", "steer", { sender: source, notify: { on: ["done", "failed", "interrupted"] } }), (error: unknown) => error instanceof Error && "status" in error && error.status === 429 && /queue the prompt/.test(error.message));
  assert.equal(runtime.calls.length, 512);
  runtime.finish("failed", "\u0001".repeat(4096)); await tick();
  assert.equal(sent.length, 1);
  sent[0].notification!.batch!.error = "\u0002".repeat(10000);
  const envelope = submissionNotification(sent[0]);
  assert.ok(Buffer.byteLength(envelope.body) <= 32768);
  const parsed = JSON.parse(envelope.body);
  assert.deepEqual(parsed.submission_ids, ids); assert.equal(parsed.submission_id, ids[0]);
  assert.equal(parsed.turn_id, queue.submission(ids[0])!.turn_id); assert.equal(parsed.status, "failed");
  assert.ok(parsed.assistant_preview.length < 4096);
  // Hit the *exact* Core boundary after JSON escaping, then capture the bytes
  // passed through the transport's real stdin stream (including any suffix).
  sent[0].notification!.batch!.error = null;
  sent[0].notification!.batch!.assistant_preview = "";
  const remaining = 32768 - Buffer.byteLength(submissionNotification(sent[0]).body);
  sent[0].notification!.batch!.assistant_preview = "\u0001".repeat(Math.floor(remaining / 6)) + "a".repeat(remaining % 6);
  const edge = submissionNotification(sent[0]); assert.equal(Buffer.byteLength(edge.body), 32768);
  const chunks: Buffer[] = [];
  const launch = ((_command: string, args: string[]) => {
    assert.deepEqual(args, ["message", "send", "--to", edge.recipient, "--idempotency-key", edge.key]);
    const stdin = new PassThrough(), child = Object.assign(new EventEmitter(), { stdin, stdout: new PassThrough(), stderr: new PassThrough(), kill: () => true });
    stdin.on("data", (chunk) => chunks.push(chunk));
    stdin.on("finish", () => queueMicrotask(() => child.emit("close", 0)));
    return child;
  }) as unknown as typeof spawn;
  await sendNotification(edge.recipient, edge.key, edge.body, launch);
  assert.equal(Buffer.concat(chunks).length, 32768);
  assert.equal(Buffer.concat(chunks).toString("utf8"), edge.body);
  assert.deepEqual(JSON.parse(Buffer.concat(chunks).toString("utf8")).submission_ids, ids);
  sent[0].notification!.batch!.submission_ids = Array.from({ length: 8192 }, (_, index) => `sub_${index.toString().padStart(36, "0")}`);
  assert.throws(() => submissionNotification(sent[0]), /membership exceeds/);
});
