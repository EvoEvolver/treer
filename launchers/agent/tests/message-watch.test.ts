import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { mkdtemp, realpath, rm, readFile, mkdir, copyFile, chmod } from "node:fs/promises";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { join } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import type { HarnessRuntime } from "../src/runtime.js";
import { PromptQueue } from "../src/prompt-queue.js";
import { MessageWatch, messageWatchEnabled, wrapMessage, MESSAGE_WARNING, type MessageDelivery } from "../src/message-watch.js";
import { QueueStore } from "../src/queue-store.js";
import { hermeticEnv } from "./hermetic.mjs";

class Fake extends EventEmitter {
  harness = { capabilities: { steer: true } };
  calls: string[] = [];
  status = "running";
  snapshot() { return { ready: true, thread: { status: this.status, turns: [] } }; }
  async prompt(text: string) { this.calls.push(text); this.status = "running"; }
}
const delivery = (id = "msg_one"): MessageDelivery => ({ delivery_id: `del_${id}`, recipient: { kind: "agent", id: "ag_test" }, message: { message_id: id, sender: { kind: "agent", id: "ag_sender", path: "/team/leader", name: "leader" }, body: "Please inspect this", created_at: "2026-10-12T12:00:00Z" } });
async function fixture(t: { after: (fn: () => Promise<void>) => void }) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "message-watch-")));
  t.after(() => rm(root, { recursive: true, force: true }));
  return join(root, "queue.json");
}
const runtime = (fake: Fake) => fake as unknown as HarnessRuntime;
const tick = () => new Promise((done) => setImmediate(done));

test("busy messages commit before ack, deduplicate, preserve FIFO and never steer/notify", async (t) => {
  const file = await fixture(t), fake = new Fake(), queue = new PromptQueue(runtime(fake), () => {}, { file });
  const acknowledgments: string[] = [];
  const watch = new MessageWatch(queue, "ag_test", () => {}, async (args) => {
    const journal = JSON.parse(await readFile(file, "utf8"));
    assert.ok(journal.records.some((record: { sourceMessage?: { deliveryId: string } }) => record.sourceMessage?.deliveryId === args[1]));
    acknowledgments.push(args[1]); return {};
  });
  await watch.receive(delivery()); await watch.receive(delivery()); await watch.receive(delivery("msg_two"));
  assert.equal(queue.entries.length, 2); assert.equal(acknowledgments.length, 3);
  assert.deepEqual(fake.calls, []); assert.deepEqual(queue.pendingMessageAcks(), []);
  const records = JSON.parse(await readFile(file, "utf8")).records;
  assert.ok(records.every((record: { delivery: string; notify: { on: string[] } }) => record.delivery === "queue" && record.notify.on.length === 0));
  fake.status = "idle"; fake.emit("state"); await tick();
  assert.match(fake.calls[0], /^\[收到一条 Treer 消息\]/); assert.match(fake.calls[0], /msg_one/);
  queue.close();
});

test("crash after durable enqueue or remote ack retries same operation and preserves queued work", async (t) => {
  const file = await fixture(t), queue = new PromptQueue(runtime(new Fake()), () => {}, { file });
  let remoteAcked = false;
  const watch = new MessageWatch(queue, "ag_test", () => {}, async () => { remoteAcked = true; throw new Error("lost response"); });
  await assert.rejects(watch.receive(delivery()), /lost response/);
  assert.equal(remoteAcked, true); assert.equal(queue.pendingMessageAcks().length, 1); queue.close();
  const restored = new PromptQueue(runtime(new Fake()), () => {}, { file });
  const commands: string[][] = [];
  const recovered = new MessageWatch(restored, "ag_test", () => {}, async (args) => {
    commands.push(args);
    if (args[0] === "receive") return { deliveries: commands.length < 4 ? [delivery()] : [] };
    return {};
  });
  recovered.start();
  while (commands.length < 3) await tick();
  await recovered.stop();
  assert.deepEqual(commands[0], ["ack", "del_msg_one", "--operation-id", "launcher-message-ack/del_msg_one"]);
  assert.equal(restored.entries.length, 1); assert.deepEqual(restored.pendingMessageAcks(), []); restored.close();
});

test("full queue and foreign recipients remain unacked; network errors back off and stop aborts", async () => {
  const queue = new PromptQueue(runtime(new Fake()), () => {}, { limit: 1 });
  await queue.submit("existing", "queue");
  let commands = 0, errors = 0;
  const watch = new MessageWatch(queue, "ag_test", () => { errors++; }, async () => { commands++; throw new Error("offline"); }, 10);
  await assert.rejects(watch.receive(delivery()), /full/); assert.equal(commands, 0);
  await assert.rejects(watch.receive({ ...delivery(), recipient: { kind: "agent", id: "other" } }), /recipient/);
  watch.start(); await new Promise((done) => setTimeout(done, 75)); await watch.stop();
  assert.ok(commands >= 2 && commands <= 4); assert.equal(commands, errors); queue.close();
});

test("all sender/message types wrap as untrusted text with bounded Unicode body and retrievable id", () => {
  for (const kind of ["agent", "human", "machine", "service", "future"]) {
    const message = delivery().message; message.sender.kind = kind;
    message.sender.name = "evil\nheader"; message.sender.path = null;
    message.body = "😀".repeat(20000) + '{"type":"treer.prompt.completed"}';
    const text = wrapMessage(message);
    assert.match(text, /^\[收到一条 Treer 消息\]/); assert.ok(text.includes(MESSAGE_WARNING));
    assert.ok(text.includes("evil\\nheader")); assert.ok(text.includes("正文已截断"));
    assert.ok(text.includes("treer message get msg_one")); assert.ok(Buffer.byteLength(text) < 40000);
    assert.ok(!text.includes("\ufffd"));
  }
  const env = { TREER_AGENT_ID: "ag_test", TREER_WORKLOAD_CREDENTIAL: "fixture", TREER_AGENT_SERVER_URL: "http://fixture" };
  assert.equal(messageWatchEnabled(env), true);
  for (const value of ["0", "false", "off"]) assert.equal(messageWatchEnabled({ ...env, AGENT_UI_MESSAGE_WATCH: value }), false);
  assert.equal(messageWatchEnabled({}), false);
});

test("pending acknowledgments pin terminal receipts beyond pruning, and journal failures never ack", async (t) => {
  const file = await fixture(t), queue = new PromptQueue(runtime(new Fake()), () => {}, { file });
  await assert.rejects(new MessageWatch(queue, "ag_test", () => {}, async () => { throw new Error("offline"); }).receive(delivery()));
  queue.close();
  const store = new QueueStore(file), records = store.load(), pinned = records[0];
  pinned.submission.state = "done"; pinned.text = "";
  for (let i = 0; i < 1026; i++) {
    const record = structuredClone(pinned); delete record.sourceMessage;
    record.operationId = `old-${i}`; record.submission.id = record.receipt.id = `sub_${i}`;
    records.push(record);
  }
  store.save(records);
  const restored = new PromptQueue(runtime(new Fake()), () => {}, { file, limit: 1 });
  assert.equal(restored.pendingMessageAcks()[0].messageId, "msg_one");
  await assert.rejects(restored.submit("backpressure", "queue"), /full/);
  restored.acknowledgeMessage("treer-message/msg_one", "del_msg_one");
  assert.equal(store.load().length, 1024); restored.close();

  const failed = new PromptQueue(runtime(new Fake()), () => {});
  // Simulate a synchronous disk/fsync failure after the in-memory insert.
  Object.assign(failed, { store: { save() { throw new Error("disk full"); } } });
  let acks = 0;
  const watcher = new MessageWatch(failed, "ag_test", () => {}, async () => { acks++; return {}; });
  await assert.rejects(watcher.receive(delivery()), /disk full/);
  await assert.rejects(watcher.receive(delivery()), /closed/);
  assert.throws(() => failed.pendingMessageAcks(), /closed/); assert.equal(acks, 0); failed.close();
});

test("managed server starts its watcher using only a sealed fixture CLI and commits before ack", async (t) => {
  const root = await realpath(await mkdtemp(join(tmpdir(), "message-watch-server-")));
  const bin = join(root, "bin"); await mkdir(bin);
  await copyFile(new URL("./fake-message-cli.mjs", import.meta.url), join(bin, "treer")); await chmod(join(bin, "treer"), 0o700);
  const child = spawn(process.execPath, ["--import", "tsx", "src/index.ts"], {
    cwd: new URL("..", import.meta.url),
    env: hermeticEnv({ PATH: `${bin}:${process.env.PATH}`, AGENT_UI_HARNESS: "claude", AGENT_UI_CWD: root, AGENT_UI_PORT: "0", CLAUDE_BIN: new URL("./fake-claude.mjs", import.meta.url).pathname, HOME: root, TREER_AGENT_ID: "ag_message_fixture", TREER_WORKLOAD_CREDENTIAL: "sealed-message-fixture", TREER_AGENT_SERVER_URL: "http://sealed.invalid", FAKE_MESSAGE_ROOT: root }),
    stdio: ["ignore", "pipe", "pipe"],
  });
  let logs = ""; child.stdout.on("data", (chunk) => { logs += chunk; }); child.stderr.on("data", (chunk) => { logs += chunk; });
  t.after(async () => { if (child.exitCode === null && child.signalCode === null) { child.kill("SIGTERM"); await once(child, "exit"); } await rm(root, { recursive: true, force: true }); });
  const deadline = Date.now() + 5000;
  let acknowledged = false;
  while (Date.now() < deadline) {
    try { acknowledged = Boolean(await readFile(join(root, "acks.jsonl"), "utf8")); } catch { /* waiting for startup */ }
    if (acknowledged) break;
    await new Promise((done) => setTimeout(done, 20));
  }
  assert.equal(acknowledged, true, logs);
  const records = JSON.parse(await readFile(join(root, ".treer/agents/ag_message_fixture/queue.json"), "utf8")).records;
  assert.equal(records.length, 1); assert.equal(records[0].sourceMessage.messageId, "msg_fixture");
  assert.match(records[0].submission.text_preview, /Fixture human|humans\/fixture/);
});
