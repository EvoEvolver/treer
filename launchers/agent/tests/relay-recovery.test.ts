import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { CodexRuntime } from "../src/harness/codex.js";
import { providerError } from "../src/harness/provider-error.js";
import { PromptQueue } from "../src/prompt-queue.js";
import type { PromptNotify, SubmissionRecord } from "../src/prompt-submission.js";
import { hermeticEnv } from "./hermetic.mjs";

async function until(condition: () => boolean) {
  const deadline = Date.now() + 5000;
  while (!condition()) {
    assert.ok(Date.now() < deadline, "sealed relay recovery timed out");
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}

test("502 terminal failure drains queued and new prompts without restart or replay", async () => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "relay-recovery-"));
  const previous = process.env;
  process.env = hermeticEnv({ FAKE_CODEX_RELAY_RECOVERY: "1", FAKE_CODEX_CALLS: join(root, "calls") });
  const runtime = new CodexRuntime(resolve(import.meta.dirname, "fake-codex.mjs"), root);
  const notifications: SubmissionRecord[] = [];
  const queue = new PromptQueue(runtime, () => {}, { file: join(root, "queue.json"), notify: async record => { notifications.push(structuredClone(record)); } });
  try {
    await runtime.start();
    const notify: PromptNotify = { to: "ag_fixture", on: ["done", "failed"] };
    const first = await queue.submit("first", "queue", { operationId: "relay-first", notify });
    const second = await queue.submit("second", "queue", { operationId: "relay-second", notify });
    await until(() => queue.submission(second.id)?.state === "done");
    assert.equal(queue.submission(first.id)?.state, "failed");
    assert.match(queue.submission(first.id)?.error ?? "", /^provider_unavailable:/);
    assert.equal(runtime.snapshot().thread?.lastError, null);
    assert.equal(runtime.snapshot().thread?.status, "idle");
    assert.equal(runtime.snapshot().thread?.turns.find(turn => turn.id === "relay-1")?.status, "failed");
    const third = await queue.submit("third", "queue", { operationId: "relay-third", notify });
    await until(() => queue.submission(third.id)?.state === "done");
    assert.equal((await queue.submit("first", "queue", { operationId: "relay-first", notify })).duplicate, true);
    await queue.flushNotifications();
    assert.deepEqual(notifications.map(record => record.submission.state), ["failed", "done", "done"]);
    const calls = readFileSync(join(root, "calls"), "utf8").trim().split("\n").map(line => JSON.parse(line));
    assert.deepEqual(calls.filter(call => call.method === "turn/start").map(call => call.params.input[0].text), ["first", "second", "third"]);
    assert.equal(calls.filter(call => call.method === "thread/start").length, 1);
    const starts = calls.flatMap((call, index) => call.method === "turn/start" ? [index] : []);
    assert.ok(calls.slice(starts[0] + 1, starts[1]).some(call => call.method === "thread/read"), "confirm native state before recovery dispatch");
  } finally { queue.close(); await runtime.stop(); process.env = previous; rmSync(root, { recursive: true, force: true }); }
});

test("a successful resumed turn does not inherit an older failure in runtime.error", async () => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "relay-history-"));
  const previous = process.env; process.env = hermeticEnv({ FAKE_CODEX_OLD_ERROR: "1" });
  const runtime = new CodexRuntime(resolve(import.meta.dirname, "fake-codex.mjs"), root, { threadId: "saved-thread" });
  try {
    await runtime.start();
    assert.equal(runtime.snapshot().thread?.lastError, null);
    assert.match(runtime.snapshot().thread?.turns[0].error ?? "", /^provider_unavailable:/);
    assert.equal(runtime.snapshot().thread?.id, "saved-thread");
  } finally { await runtime.stop(); process.env = previous; rmSync(root, { recursive: true, force: true }); }
});

for (const error of ["502 Bad Gateway", "401 Unauthorized", "403 Forbidden", "invalid model configuration", "unknown terminal failure"]) test(`new prompt after terminal ${error} is recovered or safely held`, async () => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "relay-new-"));
  const previous = process.env;
  process.env = hermeticEnv({ FAKE_CODEX_RELAY_RECOVERY: "1", FAKE_CODEX_RELAY_ERROR: error, FAKE_CODEX_CALLS: join(root, "calls") });
  const runtime = new CodexRuntime(resolve(import.meta.dirname, "fake-codex.mjs"), root);
  const queue = new PromptQueue(runtime, () => {}, { file: join(root, "queue.json") });
  try {
    await runtime.start();
    const first = await queue.submit("first", "queue", { operationId: "first" });
    await until(() => queue.submission(first.id)?.state === "failed");
    assert.equal(runtime.snapshot().thread?.lastError, providerError(error));
    const next = await queue.submit("new prompt", "queue", { operationId: "next" });
    if (error.startsWith("502")) {
      await until(() => queue.submission(next.id)?.state === "done");
      assert.equal(runtime.snapshot().thread?.lastError, null);
    } else {
      await new Promise(done => setTimeout(done, 60));
      assert.equal(queue.submission(next.id)?.state, "queued");
      assert.equal(runtime.snapshot().thread?.status, "error");
      assert.equal(readFileSync(join(root, "calls"), "utf8").split('"method":"turn/start"').length - 1, 1);
    }
    assert.equal(queue.submission(first.id)?.state, "failed");
  } finally { queue.close(); await runtime.stop(); process.env = previous; rmSync(root, { recursive: true, force: true }); }
});

test("provider error classification never returns supplier text", () => {
  for (const text of ["502", "429 rate limit", "401", "403", "configuration", "unknown"]) {
    assert.ok(!providerError(`${text} synthetic-secret`).includes("synthetic"));
  }
  for (const text of ["500 tokens exceeded", "model unavailable in this configuration", "port 502", "connection setting invalid"]) assert.ok(!providerError(text).startsWith("provider_unavailable:"));
  assert.match(providerError({ message: "synthetic-secret", codexErrorInfo: { httpConnectionFailed: { httpStatusCode: 502 } } }), /^provider_unavailable:/);
  assert.match(providerError({ message: "502 Bad Gateway", httpStatusCode: 401 }), /^provider_auth_required:/);
});

test("steer still targets the active native turn when the cached status is error", async () => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "relay-steer-"));
  const previous = process.env;
  process.env = hermeticEnv({ FAKE_CODEX_LIVE: "1", FAKE_CODEX_STATUS: "active", FAKE_CODEX_CALLS: join(root, "calls") });
  const runtime = new CodexRuntime(resolve(import.meta.dirname, "fake-codex.mjs"), root);
  const queue = new PromptQueue(runtime, () => {});
  try {
    await runtime.start(); runtime.thread!.status = "error";
    const receipt = await queue.submit("steer existing", "steer");
    assert.equal(receipt.disposition, "steered");
    const calls = readFileSync(join(root, "calls"), "utf8").trim().split("\n").map(line => JSON.parse(line));
    assert.equal(calls.filter(call => call.method === "turn/start").length, 0);
    assert.equal(calls.find(call => call.method === "turn/steer")?.params.expectedTurnId, "turn-live");
  } finally { queue.close(); await runtime.stop(); process.env = previous; rmSync(root, { recursive: true, force: true }); }
});

for (const state of ["systemError", "systemError-empty", "not-materialized"]) test(`unconfirmed native ${state} never dispatches recovery`, async () => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "relay-unconfirmed-"));
  const previous = process.env;
  process.env = hermeticEnv({ FAKE_CODEX_RELAY_RECOVERY: "1", FAKE_CODEX_RECOVERY_STATE: state, FAKE_CODEX_CALLS: join(root, "calls") });
  const runtime = new CodexRuntime(resolve(import.meta.dirname, "fake-codex.mjs"), root);
  const queue = new PromptQueue(runtime, () => {}, { file: join(root, "queue.json") });
  try {
    await runtime.start();
    const first = await queue.submit("first", "queue", { operationId: "first" });
    const second = await queue.submit("second", "queue", { operationId: "second" });
    await until(() => queue.submission(first.id)?.state === "failed");
    await new Promise(done => setTimeout(done, 80));
    assert.equal(queue.submission(second.id)?.state, "queued");
    assert.equal((await queue.submit("second", "queue", { operationId: "second" })).id, second.id);
    assert.equal(queue.submission(first.id)?.state, "failed");
    assert.equal(runtime.snapshot().thread?.turns.find(turn => turn.id === "relay-1")?.status, "failed");
    const calls = readFileSync(join(root, "calls"), "utf8").trim().split("\n").map(line => JSON.parse(line));
    assert.equal(calls.filter(call => call.method === "turn/start").length, 1);
  } finally { queue.close(); await runtime.stop(); process.env = previous; rmSync(root, { recursive: true, force: true }); }
});

test("authoritative active turn prevents recovery dispatch despite a cached terminal error", async () => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "relay-active-"));
  const previous = process.env;
  process.env = hermeticEnv({ FAKE_CODEX_RELAY_RECOVERY: "1", FAKE_CODEX_RECOVERY_ACTIVE: "1" });
  const runtime = new CodexRuntime(resolve(import.meta.dirname, "fake-codex.mjs"), root);
  const queue = new PromptQueue(runtime, () => {}, { file: join(root, "queue.json") });
  try {
    await runtime.start();
    const first = await queue.submit("first", "queue");
    const second = await queue.submit("second", "queue");
    await until(() => queue.submission(first.id)?.state === "failed" && runtime.snapshot().thread?.activeTurnId === "relay-1");
    assert.equal(queue.submission(second.id)?.state, "queued");
    await until(() => queue.submission(second.id)?.state === "done");
    assert.equal(queue.submission(first.id)?.state, "failed");
  } finally { queue.close(); await runtime.stop(); process.env = previous; rmSync(root, { recursive: true, force: true }); }
});

for (const state of ["unknown", "missing-turns"]) test(`repeated triggers cannot bypass ${state}, and later confirmed idle recovers`, async () => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "relay-repeat-"));
  const stateFile = join(root, "state"); writeFileSync(stateFile, state);
  const previous = process.env;
  process.env = hermeticEnv({ FAKE_CODEX_RELAY_RECOVERY: "1", FAKE_CODEX_RECOVERY_STATE_FILE: stateFile, FAKE_CODEX_CALLS: join(root, "calls") });
  const runtime = new CodexRuntime(resolve(import.meta.dirname, "fake-codex.mjs"), root);
  const queue = new PromptQueue(runtime, () => {}, { file: join(root, "queue.json") });
  const calls = () => readFileSync(join(root, "calls"), "utf8").trim().split("\n").map(line => JSON.parse(line));
  try {
    await runtime.start();
    const first = await queue.submit("first", "queue", { operationId: "first" });
    const second = await queue.submit("second", "queue", { operationId: "second" });
    await until(() => queue.submission(first.id)?.state === "failed");
    await new Promise(done => setTimeout(done, 40));
    const third = await queue.submit("third", "queue"); runtime.emit("state");
    await new Promise(done => setTimeout(done, 40));
    assert.equal(queue.submission(second.id)?.state, "queued"); assert.equal(queue.submission(third.id)?.state, "queued");
    assert.equal(calls().filter(call => call.method === "turn/start").length, 1);
    assert.equal((await queue.submit("second", "queue", { operationId: "second" })).id, second.id);
    writeFileSync(stateFile, "idle"); runtime.emit("state");
    await until(() => queue.submission(third.id)?.state === "done");
    assert.equal(queue.submission(second.id)?.state, "done"); assert.equal(queue.submission(first.id)?.state, "failed");
    assert.deepEqual(calls().filter(call => call.method === "turn/start").map(call => call.params.input[0].text), ["first", "second", "third"]);
  } finally { queue.close(); await runtime.stop(); process.env = previous; rmSync(root, { recursive: true, force: true }); }
});
