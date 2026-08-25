import { EventEmitter } from "node:events";
import assert from "node:assert/strict";
import test from "node:test";

import { createCodexBackend } from "./backend.mjs";
import { startCodexAis } from "./sidecar.mjs";

function fakeRpc(handlers) {
  return {
    events: new EventEmitter(),
    request: async (method, params) => handlers[method](params),
    notify() {},
    close() {},
  };
}

test("Codex backend binds one thread and pages native turns", async () => {
  const started = [];
  const rpc = fakeRpc({
    initialize: async () => ({ userAgent: "codex" }),
    "thread/start": async (params) => {
      started.push("thread");
      assert.equal(params.sandbox, "workspace-write");
      assert.equal(params.approvalPolicy, "never");
      return { thread: { id: "thr_1" } };
    },
    "thread/read": async ({ threadId }) => {
      assert.equal(threadId, "thr_1");
      return {
        thread: {
          id: "thr_1",
          turns: [
            { id: "t1", items: [
              { id: "u1", type: "userMessage", text: "first" },
              { id: "a1", type: "agentMessage", text: "ok" },
            ] },
            { id: "t2", items: [
              { id: "u2", type: "userMessage", text: "second" },
            ] },
          ],
        },
      };
    },
    "turn/start": async ({ threadId, input }) => {
      assert.equal(threadId, "thr_1");
      assert.equal(input[0].text, "hello");
      return { turn: { id: "turn_9" } };
    },
    "turn/interrupt": async ({ threadId, turnId }) => {
      assert.equal(threadId, "thr_1");
      assert.equal(turnId, "turn_9");
      return {};
    },
  });
  const backend = createCodexBackend(rpc, { cwd: "/tmp" });
  await backend.start();
  assert.equal(backend.threadId(), "thr_1");
  assert.equal(started.length, 1);
  const entries = await backend.entries();
  assert.deepEqual(entries.map((entry) => entry.id), ["u1", "a1", "u2"]);
  await backend.prompt("hello");
  await backend.abort();
});

test("Codex backend returns an empty transcript before the first user message", async () => {
  const rpc = fakeRpc({
    initialize: async () => ({}),
    "thread/start": async () => ({ thread: { id: "thr_1" } }),
    "thread/read": async () => {
      throw Object.assign(new Error("thread thr_1 is not materialized yet; includeTurns is unavailable before first user message"), {
        code: -32600,
      });
    },
  });
  const backend = createCodexBackend(rpc, { cwd: "/tmp" });
  await backend.start();
  assert.deepEqual(await backend.entries(), []);
});

test("Codex AIS serves one turn per page", async () => {
  const rpc = fakeRpc({
    initialize: async () => ({}),
    "thread/start": async () => ({ thread: { id: "thr_1" } }),
    "thread/read": async () => ({
      thread: {
        turns: [
          { items: [
            { id: "u1", type: "userMessage", text: "first" },
            { id: "a1", type: "agentMessage", text: "ok" },
          ] },
          { items: [{ id: "u2", type: "userMessage", text: "second" }] },
        ],
      },
    }),
    "turn/start": async () => ({ turn: { id: "turn_1" } }),
  });
  const previous = process.env.AIS_AUTO_REGISTER;
  process.env.AIS_AUTO_REGISTER = "0";
  delete process.env.TREER_AGENT_ID;
  const runtime = await startCodexAis({
    rpc,
    child: { kill() {} },
    backend: createCodexBackend(rpc, { cwd: "/tmp" }),
  });
  try {
    const base = `http://127.0.0.1:${runtime.port}`;
    const page = await (await fetch(`${base}/v1/transcript`)).json();
    assert.equal(page.page, 0);
    assert.equal(page.page_count, 2);
    assert.deepEqual(page.entries.map((entry) => entry.id), ["u1", "a1"]);
    const next = await (await fetch(`${base}/v1/transcript?page=1`)).json();
    assert.deepEqual(next.entries.map((entry) => entry.id), ["u2"]);
  } finally {
    await runtime.shutdown();
    if (previous === undefined) delete process.env.AIS_AUTO_REGISTER;
    else process.env.AIS_AUTO_REGISTER = previous;
  }
});
